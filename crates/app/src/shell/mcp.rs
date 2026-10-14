//! MCP tools for agents: the app end of `Hangar mcp-bridge`. Each request from the host
//! (`crate::mcp_host`) is routed to the device API of its workspace, or to the orchestration
//! tools (`orchestration.rs`); device I/O stays off the UI thread and every reply goes back
//! through its oneshot.

use anyhow::Context as _;

use crate::agents::{self, AgentExtras, BridgeLaunch};
use crate::device_panel::Platform;
use crate::mcp_host::{self, McpHandle, McpRequest, ScreenFrame};

use super::device_api::DeviceStatusInfo;
use super::*;

/// Starts the host on the private socket. Agents simply get no device tools if this fails.
pub(super) fn start(window: &mut Window, cx: &mut Context<HangarApp>) -> Option<McpHandle> {
    let started = paths::socket_path().and_then(|socket| mcp_host::start(&socket));
    match started {
        Ok((handle, mut requests)) => {
            tracing::info!(socket = %handle.socket_path().display(), "mcp host listening");
            cx.spawn_in(window, async move |this, cx| {
                while let Some(request) = requests.recv().await {
                    if this
                        .update_in(cx, |this, window, cx| {
                            this.dispatch_mcp(request, window, cx)
                        })
                        .is_err()
                    {
                        break;
                    }
                }
            })
            .detach();
            Some(handle)
        }
        Err(err) => {
            tracing::error!("mcp host not started, agents get no device tools: {err:#}");
            None
        }
    }
}

fn status(info: DeviceStatusInfo) -> hangar_mcp::DeviceStatus {
    hangar_mcp::DeviceStatus {
        platform: info.platform,
        name: info.name,
        id: info.id,
        booted: info.booted,
        screen_pt: info.screen_pt,
        scale: info.scale,
        orientation: info.orientation,
    }
}

/// Sends `task`'s result through `reply` once it resolves.
fn answer<T: 'static, U: Send + 'static>(
    task: Task<anyhow::Result<T>>,
    reply: mcp_host::Reply<U>,
    map: impl FnOnce(T) -> U + 'static,
    cx: &mut Context<HangarApp>,
) {
    cx.spawn(async move |_, _| {
        let _ = reply.send(task.await.map(map));
    })
    .detach();
}

impl HangarApp {
    fn dispatch_mcp(&mut self, request: McpRequest, window: &mut Window, cx: &mut Context<Self>) {
        match request {
            McpRequest::Signal {
                ws,
                signal,
                transcript,
            } => {
                if let Some(live) = self.live.sessions.get_mut(&ws)
                    && matches!(live.surface, AgentSurface::Terminal { .. })
                {
                    tracing::debug!(workspace = %ws, signal = signal.as_str(), "agent signal");
                    live.signal = Some(signal);
                    if transcript.is_some() {
                        live.transcript_path = transcript;
                    }
                    self.sync_attention(cx);
                    cx.notify();
                }
            }
            McpRequest::Ask {
                ws,
                question,
                choices,
                reply,
            } => self.on_question(ws, question, choices, reply, window, cx),
            McpRequest::Status { ws, reply } => {
                let task = self.device_status(ws, cx);
                answer(task, reply, status, cx);
            }
            McpRequest::Frame { ws, reply } => {
                let task = self.latest_frame(ws, cx);
                answer(
                    task,
                    reply,
                    |f| ScreenFrame {
                        frame: f.frame,
                        width_pt: f.width_pt,
                        height_pt: f.height_pt,
                    },
                    cx,
                );
            }
            McpRequest::Input { ws, input, reply } => {
                let task = self.send_input(ws, input, cx);
                answer(task, reply, |()| (), cx);
            }
            McpRequest::Text { ws, text, reply } => {
                let task = self.send_text(ws, text, cx);
                answer(task, reply, |()| (), cx);
            }
            McpRequest::Logs {
                ws,
                lines,
                min_level,
                process,
                reply,
            } => {
                let task = self.recent_log_entries(ws, lines, min_level, process, cx);
                answer(task, reply, |entries| entries, cx);
            }
            McpRequest::Launch { ws, app_id, reply } => {
                let task = self.launch_app(ws, app_id, cx);
                answer(task, reply, |()| (), cx);
            }
            McpRequest::BuildAndRun { ws, reply } => {
                let task = self.run_workspace(ws, window, cx);
                answer(task, reply, |summary| summary, cx);
            }
            McpRequest::Children { ws, reply } => {
                let _ = reply.send(self.child_workspaces(ws, cx));
            }
            McpRequest::CreateChild {
                ws,
                task,
                name,
                agent,
                reply,
            } => self.create_child(ws, task, name, agent, reply, window, cx),
            McpRequest::PromptChild {
                ws,
                child,
                text,
                reply,
            } => {
                let _ = reply.send(self.prompt_child(ws, &child, &text, window, cx));
            }
            McpRequest::Transcript {
                ws,
                child,
                last,
                reply,
            } => {
                use super::orchestration::{TranscriptSource, read_claude_transcript};
                match self.child_transcript(ws, &child, last, cx) {
                    Ok((name, TranscriptSource::Ready(entries))) => {
                        let _ = reply.send(Ok((name, entries)));
                    }
                    Ok((name, TranscriptSource::ClaudeFile(path))) => {
                        cx.background_executor()
                            .spawn(async move {
                                let _ = reply.send(
                                    read_claude_transcript(&path, last)
                                        .map(|entries| (name, entries)),
                                );
                            })
                            .detach();
                    }
                    Err(err) => {
                        let _ = reply.send(Err(err));
                    }
                }
            }
        }
    }

    /// `HANGAR_DEVICE_*` (and `ANDROID_SERIAL`) for the workspace's assigned device, as far as it
    /// is known: an Android serial only exists once an instance has booted.
    fn device_env(&self, ws: WorkspaceId, cx: &App) -> Vec<(String, String)> {
        let Some(assignment) = self.device_pool.read(cx).assignment(ws) else {
            return Vec::new();
        };
        let mut env = Vec::new();
        match assignment.platform {
            Platform::Ios => {
                env.push(("HANGAR_DEVICE_PLATFORM".to_owned(), "ios".to_owned()));
                if let Some(udid) = &assignment.own {
                    env.push(("HANGAR_DEVICE_ID".to_owned(), udid.clone()));
                }
            }
            Platform::Android => {
                env.push(("HANGAR_DEVICE_PLATFORM".to_owned(), "android".to_owned()));
                if let Some(serial) = &assignment.serial {
                    env.push(("HANGAR_DEVICE_ID".to_owned(), serial.clone()));
                    env.push(("ANDROID_SERIAL".to_owned(), serial.clone()));
                }
            }
        }
        env
    }

    /// Args, env and files that give `adapter`'s agent the device tools of `workspace`, and
    /// the Ollama provider when it is on.
    pub(super) fn agent_extras(
        &self,
        agent_id: &str,
        workspace: &Workspace,
        cx: &App,
    ) -> anyhow::Result<AgentExtras> {
        let mut extras = self.mcp_extras(agent_id, workspace, cx)?;
        if let Some(provider) = self.agent_setup.ollama.opencode_provider() {
            agents::apply_ollama(&mut extras, agent_id, &provider)?;
        }
        Ok(extras)
    }

    fn mcp_extras(
        &self,
        agent_id: &str,
        workspace: &Workspace,
        cx: &App,
    ) -> anyhow::Result<AgentExtras> {
        let Some(mcp) = &self.mcp else {
            return Ok(AgentExtras::default());
        };
        let bridge = BridgeLaunch {
            exe: std::env::current_exe().context("cannot locate the hangar executable")?,
            socket: mcp.socket_path().to_owned(),
            session: mcp.register(workspace.id),
            config_dir: paths::mcp_config_dir().context("no Hangar data directory")?,
            workspace: workspace.id,
        };
        agents::mcp_launch(agent_id, &bridge, &self.device_env(workspace.id, cx))
    }

    /// Forgets the workspace's MCP session and its Gemini config file (archive, delete).
    pub(super) fn close_mcp(&self, id: WorkspaceId) {
        if let Some(mcp) = &self.mcp {
            mcp.unregister(id);
        }
        if let Some(dir) = paths::mcp_config_dir() {
            let _ = std::fs::remove_file(dir.join(format!("{id}.gemini.json")));
        }
    }
}
