//! Structured (chat) sessions: attach, resume, cost and the workspace agent.

use super::*;

impl HangarApp {
    pub(in crate::shell) fn attach_chat_session(
        &mut self,
        started: ops::StartedAcpSession,
        agent_name: SharedString,
        waker: TerminalWaker,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let ops::StartedAcpSession {
            session,
            acp,
            events,
            approval,
            record_error,
        } = started;
        let workspace_id = session.workspace_id;
        tracing::info!(
            workspace = %workspace_id,
            session = %session.id,
            program = %acp.program().display(),
            pid = acp.pid(),
            acp_session = acp.session_id(),
            "structured session started"
        );
        if let Some(err) = record_error {
            self.report(&err, window, cx);
        }
        if !self.keep_started(workspace_id, &session, window, cx) {
            acp.kill();
            return;
        }
        let chat = ops::SavedChat {
            agent: acp.agent_id().to_owned(),
            session: acp.session_id().to_owned(),
        };
        self.live.saved_chats.insert(workspace_id, chat.clone());
        self.persist(window, cx, move |store| {
            ops::save_chat(store, workspace_id, &chat)
        });
        let root = self
            .work
            .projects
            .workspace(workspace_id)
            .map(|ws| ws.worktree_path.clone())
            .unwrap_or_default();
        let view = {
            let acp = Arc::clone(&acp);
            let name = agent_name.clone();
            cx.new(|cx| ChatView::new(acp, events, waker, name, root, window, cx))
        };
        view.update(cx, |chat, _| chat.set_approval(approval));
        if acp.resumed() {
            view.update(cx, |chat, _| {
                chat.add_notice(false, "Resumed the previous conversation")
            });
        } else if let Some(reason) = acp.resume_error() {
            let text =
                format!("Could not resume the previous conversation ({reason}); this is a new one");
            view.update(cx, |chat, _| chat.add_notice(true, text));
        }
        let agent_id = acp.agent_id().to_owned();
        let exit = cx.subscribe_in(
            &view,
            window,
            move |this, view, event: &ChatEvent, window, cx| match event {
                ChatEvent::StateChanged => {
                    if let Some(entry) = this.live.diffs.get(&workspace_id) {
                        let diff = entry.diff.clone();
                        view.update(cx, |chat, cx| chat.set_diff(diff, cx));
                    }
                    let cost = view.read(cx).usage().cost();
                    this.save_session_cost(workspace_id, cost, window, cx);
                    let spend = view.update(cx, |chat, _| chat.take_spend());
                    this.book_spend(workspace_id, spend, window, cx);
                    cx.notify()
                }
                ChatEvent::Exited { code, stderr_tail } => {
                    let spend = view.update(cx, |chat, _| chat.take_spend());
                    this.book_spend(workspace_id, spend, window, cx);
                    if !stderr_tail.is_empty() {
                        tracing::info!(workspace = %workspace_id, "agent stderr: {stderr_tail}");
                    }
                    this.on_session_exit(workspace_id, ops::acp_exit_status(*code), window, cx);
                }
                ChatEvent::OpenFile(path) => this.open_in_files(workspace_id, path, window, cx),
                ChatEvent::ConfigChosen { config_id, value } => {
                    let (agent, id, value) = (agent_id.clone(), config_id.clone(), value.clone());
                    this.persist(window, cx, move |store| {
                        ops::remember_agent_config(store, &agent, &id, &value)
                    });
                }
                ChatEvent::ModeChosen(mode) => {
                    let (agent, mode) = (agent_id.clone(), mode.clone());
                    this.persist(window, cx, move |store| {
                        ops::remember_workspace_mode(store, workspace_id, &agent, &mode)
                    });
                }
            },
        );
        let live = LiveSession {
            session,
            agent_name,
            surface: AgentSurface::Chat { acp, view },
            activity: Default::default(),
            signal: None,
            transcript_path: None,
            saved_cost: None,
            _exit: exit,
        };
        self.insert_live_session(workspace_id, live, window, cx);
    }

    /// The saved conversation of `workspace_id` if `agent_id` ran it.
    pub(in crate::shell) fn resumable_chat(
        &self,
        workspace_id: WorkspaceId,
        agent_id: &str,
    ) -> Option<String> {
        self.live
            .saved_chats
            .get(&workspace_id)
            .filter(|chat| chat.agent == agent_id)
            .map(|chat| chat.session.clone())
    }

    /// Forgets the workspace's conversation and agent here; the store forgets them with
    /// [`ops::forget_chat`] when the workspace is deleted.
    pub(in crate::shell) fn forget_chat(&mut self, workspace_id: WorkspaceId) {
        self.live.saved_chats.remove(&workspace_id);
        self.live.workspace_agents.remove(&workspace_id);
    }

    /// Saves what the workspace's session has cost when the agent reported a new total, so the
    /// day's total survives a relaunch.
    fn save_session_cost(
        &mut self,
        workspace_id: WorkspaceId,
        cost: Option<hangar_agents::acp::protocol::Cost>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(cost) = cost.as_ref().map(crate::chat_view::stored_cost) else {
            return;
        };
        let Some(live) = self.live.sessions.get_mut(&workspace_id) else {
            return;
        };
        if live.saved_cost.as_ref() == Some(&cost) {
            return;
        }
        live.saved_cost = Some(cost.clone());
        let session = live.session.id;
        self.persist(window, cx, move |store| {
            ops::save_session_cost(store, session, &cost)
        });
    }

    /// Adds what the workspace's chat spent to the usage ledger.
    fn book_spend(
        &mut self,
        workspace_id: WorkspaceId,
        spend: Option<crate::chat_view::Spend>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(spend) = spend else {
            return;
        };
        let Some(live) = self.live.sessions.get(&workspace_id) else {
            return;
        };
        let rows = hangar_usecases::usage::book_turn(
            live.session.id,
            hangar_core::Timestamp::now(),
            crate::chat_view::turn_spend(&spend),
            spend.model.as_deref(),
        );
        if rows.is_empty() {
            return;
        }
        self.persist(window, cx, move |store| ops::record_usage(store, &rows));
    }

    /// The agent to start in `workspace_id`: the one last chosen or run there, else the default (Settings › Agents).
    pub(in crate::shell) fn agent_for(&self, workspace_id: WorkspaceId) -> String {
        self.live
            .workspace_agents
            .get(&workspace_id)
            .filter(|id| agents::preset(id).is_some())
            .cloned()
            .unwrap_or_else(|| self.agent_setup.agent.to_string())
    }

    pub(in crate::shell) fn remember_workspace_agent(
        &mut self,
        workspace_id: WorkspaceId,
        agent_id: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self
            .live
            .workspace_agents
            .get(&workspace_id)
            .map(String::as_str)
            == Some(agent_id)
        {
            return;
        }
        self.live
            .workspace_agents
            .insert(workspace_id, agent_id.to_owned());
        let agent = agent_id.to_owned();
        self.persist(window, cx, move |store| {
            ops::save_workspace_agent(store, workspace_id, &agent)
        });
    }

    /// Shows `path` (absolute, inside the workspace's worktree) in the Files tab.
    fn open_in_files(
        &mut self,
        workspace_id: WorkspaceId,
        path: &std::path::Path,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(ws) = self.work.projects.workspace(workspace_id) else {
            return;
        };
        let Ok(relative) = path.strip_prefix(&ws.worktree_path) else {
            tracing::info!("not opening {}: outside the worktree", path.display());
            return;
        };
        let relative = relative.to_owned();
        if let Some(pane) = self.layout.panes.pane_of(workspace_id) {
            self.focus_pane(pane, window, cx);
        }
        self.set_files_open(true, window, cx);
        self.files_panel
            .update(cx, |panel, cx| panel.open(relative, cx));
    }
}
