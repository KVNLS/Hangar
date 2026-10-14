//! The MCP orchestration tools on the GPUI side: a lead agent lists, creates, prompts and reads
//! the workspaces it created. The guard rails are `hangar_usecases::orchestration`; everything
//! an agent does here is shown in its own pane.

use anyhow::Context as _;
use hangar_mcp::{ChildWorkspace, CreatedWorkspace, StartOutcome, ToolError, TranscriptEntry};
use hangar_usecases::orchestration;

use super::toast::ToastKind;
use super::*;
use crate::mcp_host::Reply;

/// Terminal lines given for a terminal agent without a structured transcript.
const SCREEN_LINES: usize = 120;

/// What `agent_transcript` reads.
pub(super) enum TranscriptSource {
    Ready(Vec<TranscriptEntry>),
    /// Claude Code's session file, checked by `mcp_host::accept_transcript`.
    ClaudeFile(PathBuf),
}

/// The last `last` entries of the Claude Code session file at `path` (its tail only: the file
/// grows with the session).
pub(super) fn read_claude_transcript(
    path: &std::path::Path,
    last: usize,
) -> anyhow::Result<Vec<TranscriptEntry>> {
    use std::io::{Read as _, Seek as _, SeekFrom};
    const TAIL: u64 = 4 << 20;
    let mut file =
        std::fs::File::open(path).with_context(|| format!("cannot open {}", path.display()))?;
    let len = file.metadata()?.len();
    file.seek(SeekFrom::Start(len.saturating_sub(TAIL)))?;
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes)?;
    let text = String::from_utf8_lossy(&bytes);
    // A cut first line is not JSON and is skipped by the parser.
    let mut entries: Vec<TranscriptEntry> = hangar_agents::claude_transcript::parse(&text)
        .into_iter()
        .map(|entry| match entry {
            hangar_agents::claude_transcript::Entry::User(text) => TranscriptEntry::User(text),
            hangar_agents::claude_transcript::Entry::Agent(text) => TranscriptEntry::Agent(text),
            hangar_agents::claude_transcript::Entry::Tool { title, status } => {
                TranscriptEntry::Tool {
                    title,
                    status: status.to_owned(),
                }
            }
        })
        .collect();
    let skip = entries.len().saturating_sub(last);
    entries.drain(..skip);
    Ok(entries)
}

/// Terminal lines as text for an agent: rows that only draw boxes or rules are dropped and runs
/// of blank rows collapse to one.
pub(super) fn screen_text(lines: &[String]) -> String {
    let mut out: Vec<&str> = Vec::new();
    for line in lines {
        let line = line.trim_end();
        let drawn = !line.is_empty()
            && line
                .chars()
                .all(|c| c.is_whitespace() || ('\u{2500}'..='\u{259F}').contains(&c));
        if drawn || (line.is_empty() && out.last().is_none_or(|l| l.is_empty())) {
            continue;
        }
        out.push(line);
    }
    out.join("\n").trim().to_owned()
}

impl HangarApp {
    /// Turns the orchestration tools on or off for every agent, without saving.
    pub(super) fn apply_orchestration(&mut self, on: bool) {
        self.agent_setup.orchestration = on;
        if let Some(mcp) = &self.mcp {
            mcp.set_orchestration(on);
        }
    }

    pub(super) fn set_orchestration(
        &mut self,
        on: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.apply_orchestration(on);
        self.persist(window, cx, move |store| ops::save_orchestration(store, on));
        cx.notify();
    }

    /// Every workspace, active and archived.
    fn every_workspace(&self) -> Vec<&Workspace> {
        self.work
            .projects
            .entries()
            .iter()
            .flat_map(|e| e.workspaces.iter())
            .collect()
    }

    /// Active workspaces created by an agent: their diffs are kept for `workspaces_list`.
    pub(super) fn orchestrated_workspaces(&self) -> Vec<WorkspaceId> {
        self.work
            .projects
            .entries()
            .iter()
            .flat_map(|e| e.active())
            .filter(|ws| ws.parent.is_some())
            .map(|ws| ws.id)
            .collect()
    }

    /// The name of the workspace whose agent created `ws`, if any.
    pub(super) fn parent_name(&self, ws: &Workspace) -> Option<String> {
        let parent = self.work.projects.workspace(ws.parent?)?;
        Some(parent.name.clone())
    }

    fn ensure_orchestration(&self) -> anyhow::Result<()> {
        if self.agent_setup.orchestration {
            Ok(())
        } else {
            Err(anyhow::Error::new(ToolError::Failed(
                "orchestration tools are off in Hangar's settings".into(),
            )))
        }
    }

    /// The display name of the agent running in `ws`, else of the one it last started.
    fn agent_label(&self, ws: WorkspaceId) -> Option<String> {
        self.live
            .sessions
            .get(&ws)
            .map(|l| l.agent_name.to_string())
            .or_else(|| {
                let id = self.live.workspace_agents.get(&ws)?;
                agents::preset(id).map(|p| p.display_name().to_owned())
            })
    }

    pub(super) fn child_workspaces(
        &self,
        ws: WorkspaceId,
        cx: &App,
    ) -> anyhow::Result<Vec<ChildWorkspace>> {
        self.ensure_orchestration()?;
        let all = self.every_workspace();
        Ok(orchestration::children(all.iter().copied(), ws)
            .into_iter()
            .map(|child| ChildWorkspace {
                id: child.id.to_string(),
                name: child.name.clone(),
                task: child.task.clone(),
                branch: child.branch.clone(),
                agent: self.agent_label(child.id),
                state: self.agent_state(child.id, cx).label().to_owned(),
                diff: self
                    .diff_counts(child.id, cx)
                    .map(|c| (c.insertions, c.deletions)),
                queued: self.queued_ahead(child.id).is_some(),
            })
            .collect())
    }

    /// Creates a child of `ws` in its project and starts its agent through the queue. The
    /// reply comes once the agent started, failed to start or was queued.
    #[allow(clippy::too_many_arguments)]
    pub(super) fn create_child(
        &mut self,
        ws: WorkspaceId,
        task: String,
        name: Option<String>,
        agent: Option<String>,
        reply: Reply<CreatedWorkspace>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let prepared = (|| {
            self.ensure_orchestration()?;
            let parent = self
                .work
                .projects
                .workspace(ws)
                .cloned()
                .ok_or_else(|| anyhow!("this agent's workspace is no longer open"))?;
            let in_flight = self.work.child_creates.get(&ws).copied().unwrap_or(0);
            orchestration::check_create(self.every_workspace(), ws, in_flight)?;
            let spec = orchestration::child_workspace(&parent, &task, name.as_deref())?;
            let agent_id = match agent {
                Some(id) => self.check_agent(&id)?,
                None => self.agent_for(ws),
            };
            anyhow::Ok((parent, spec, agent_id))
        })();
        let (parent, spec, agent_id) = match prepared {
            Ok(prepared) => prepared,
            Err(err) => {
                let _ = reply.send(Err(err));
                return;
            }
        };
        let agent_name = agents::preset(&agent_id)
            .map_or_else(|| agent_id.clone(), |p| p.display_name().to_owned());
        let child_name = spec.name.clone();
        tracing::info!(parent = %ws, name = %child_name, agent = %agent_id, "agent creates a workspace");
        *self.work.child_creates.entry(ws).or_default() += 1;
        let spawned =
            self.spawn_workspace(parent.project_id, spec, agent_id, None, false, window, cx);
        cx.spawn_in(window, async move |this, cx| {
            let result = match spawned.await {
                Ok(spawned) => {
                    let queued = spawned.queued;
                    let started = spawned.start.await;
                    let start = match queued {
                        Some(ahead) => StartOutcome::Queued { ahead },
                        None if started => StartOutcome::Started,
                        None => StartOutcome::Failed,
                    };
                    Ok(CreatedWorkspace {
                        id: spawned.workspace.id.to_string(),
                        name: spawned.workspace.name.clone(),
                        branch: spawned.workspace.branch.clone(),
                        agent: agent_name.clone(),
                        start,
                    })
                }
                Err(err) => Err(err),
            };
            let _ = this.update_in(cx, |this, window, cx| {
                if let Some(count) = this.work.child_creates.get_mut(&ws) {
                    *count = count.saturating_sub(1);
                    if *count == 0 {
                        this.work.child_creates.remove(&ws);
                    }
                }
                match &result {
                    Ok(created) => {
                        this.sync_diffs(cx);
                        this.tell_parent(
                            ws,
                            false,
                            format!("Created workspace “{}” with {agent_name}", created.name),
                            window,
                            cx,
                        );
                    }
                    Err(err) => this.tell_parent(
                        ws,
                        true,
                        format!("Could not create workspace “{child_name}”: {err:#}"),
                        window,
                        cx,
                    ),
                }
                cx.notify();
            });
            let _ = reply.send(result);
        })
        .detach();
    }

    /// `id` when it is a known, installed agent preset.
    fn check_agent(&self, id: &str) -> anyhow::Result<String> {
        let Some(preset) = agents::preset(id) else {
            let known: Vec<&str> = self
                .agent_setup
                .agent_choices
                .iter()
                .map(|c| c.id.as_str())
                .collect();
            anyhow::bail!("unknown agent {id:?}; Hangar knows {}", known.join(", "));
        };
        if self
            .agent_setup
            .agent_choices
            .iter()
            .any(|c| c.id == id && !c.installed)
        {
            anyhow::bail!("{} is not installed on this Mac", preset.display_name());
        }
        Ok(id.to_owned())
    }

    /// Sends `text` to the agent of `ws`'s child `query`. Returns the child's name.
    pub(super) fn prompt_child(
        &mut self,
        ws: WorkspaceId,
        query: &str,
        text: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> anyhow::Result<String> {
        self.ensure_orchestration()?;
        let child = orchestration::find_child(self.every_workspace(), ws, query)?.clone();
        let live = self.live_agent(&child)?;
        live.send_text(text, cx)?;
        tracing::info!(parent = %ws, child = %child.id, "agent prompted a workspace");
        self.tell_parent(ws, false, format!("Prompted “{}”", child.name), window, cx);
        Ok(child.name)
    }

    /// The last `last` messages of the structured agent in `ws`'s child `query`.
    /// Where `child`'s transcript comes from: a structured agent's chat, a terminal Claude Code's
    /// session file (read off the UI thread by the caller), or a terminal's recent text.
    pub(super) fn child_transcript(
        &self,
        ws: WorkspaceId,
        query: &str,
        last: usize,
        cx: &App,
    ) -> anyhow::Result<(String, TranscriptSource)> {
        self.ensure_orchestration()?;
        let child = orchestration::find_child(self.every_workspace(), ws, query)?;
        let live = self.live_agent(child)?;
        let source = match &live.surface {
            AgentSurface::Chat { view, .. } => {
                TranscriptSource::Ready(view.read(cx).transcript(last))
            }
            AgentSurface::Terminal { .. } if live.transcript_path.is_some() => {
                TranscriptSource::ClaudeFile(live.transcript_path.clone().unwrap_or_default())
            }
            AgentSurface::Terminal { view, .. } => {
                let lines = view
                    .read(cx)
                    .session()
                    .terminal()
                    .recent_lines(SCREEN_LINES);
                TranscriptSource::Ready(vec![TranscriptEntry::Screen(screen_text(&lines))])
            }
        };
        Ok((child.name.clone(), source))
    }

    /// The running agent of `child`.
    fn live_agent(&self, child: &Workspace) -> anyhow::Result<&LiveSession> {
        if let Some(ahead) = self.queued_ahead(child.id) {
            anyhow::bail!(
                "the agent of “{}” has not started yet: it waits behind {ahead} other start(s)",
                child.name
            );
        }
        self.live
            .sessions
            .get(&child.id)
            .filter(|l| !l.session.status.is_terminal())
            .ok_or_else(|| anyhow!("no agent runs in “{}”", child.name))
    }

    /// Shows what `ws`'s agent did through the orchestration tools in its pane: a line in a
    /// structured agent's chat, else a toast (an error notification for failures).
    fn tell_parent(
        &mut self,
        ws: WorkspaceId,
        error: bool,
        text: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match self.live.sessions.get(&ws).map(|l| &l.surface) {
            Some(AgentSurface::Chat { view, .. }) => view.update(cx, |chat, cx| {
                chat.add_notice(error, text);
                cx.notify();
            }),
            _ if error => window.push_notification(Notification::error(text), cx),
            _ => self.show_toast(ToastKind::Info, text, cx),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::screen_text;

    #[test]
    fn screen_text_drops_drawn_rows_and_extra_blanks() {
        let lines: Vec<String> = [
            "",
            "╭──────╮",
            "│ hi   │",
            "",
            "",
            "╰──────╯",
            "$ ls",
            "a.txt",
            "",
        ]
        .iter()
        .map(|l| (*l).to_owned())
        .collect();
        assert_eq!(screen_text(&lines), "│ hi   │\n\n$ ls\na.txt");
    }
}
