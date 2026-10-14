//! Agent surfaces, live sessions and the small types the shell passes around.

use super::*;

#[derive(Clone, Debug, PartialEq)]
pub(super) struct AgentOption {
    pub(super) id: SharedString,
    pub(super) title: SharedString,
}

impl SelectItem for AgentOption {
    type Value = SharedString;

    fn title(&self) -> SharedString {
        self.title.clone()
    }

    fn value(&self) -> &Self::Value {
        &self.id
    }
}

pub(super) type AgentSelect = SelectState<Vec<AgentOption>>;

/// What runs a workspace's agent and how the pane shows it.
pub(super) enum AgentSurface {
    /// An interactive CLI in a terminal. Dropping the handle kills the program.
    Terminal {
        handle: AgentHandle,
        view: Entity<TerminalView>,
    },
    /// A structured agent over ACP, shown as a chat.
    Chat {
        acp: Arc<AcpSession>,
        view: Entity<ChatView>,
    },
}

pub(super) struct LiveSession {
    pub(super) session: Session,
    pub(super) agent_name: SharedString,
    pub(super) surface: AgentSurface,
    pub(super) activity: status::Activity,
    /// What a terminal agent last reported through its hooks (Claude Code); `None` without hooks.
    pub(super) signal: Option<hangar_core::AgentSignal>,
    /// A terminal Claude Code's session transcript, as its hooks reported it (checked).
    pub(super) transcript_path: Option<PathBuf>,
    /// The session's cost as last saved (structured agents that report one).
    pub(super) saved_cost: Option<hangar_core::changes::Cost>,
    pub(super) _exit: Subscription,
}

impl LiveSession {
    /// Kills the program if it still runs. Returns the session to record as stopped, if any.
    pub(super) fn stop(&self) -> Option<hangar_core::SessionId> {
        if self.session.status.is_terminal() {
            return None;
        }
        match &self.surface {
            AgentSurface::Terminal { handle, .. } => {
                if let Err(err) = handle.session().kill() {
                    tracing::warn!(%err, session = %self.session.id, "failed to kill agent");
                }
            }
            AgentSurface::Chat { acp, .. } => acp.kill(),
        }
        Some(self.session.id)
    }

    pub(super) fn view(&self) -> AnyView {
        match &self.surface {
            AgentSurface::Terminal { view, .. } => view.clone().into(),
            AgentSurface::Chat { view, .. } => view.clone().into(),
        }
    }

    pub(super) fn focus_handle(&self, cx: &App) -> FocusHandle {
        match &self.surface {
            AgentSurface::Terminal { view, .. } => view.focus_handle(cx),
            AgentSurface::Chat { view, .. } => view.focus_handle(cx),
        }
    }

    /// Types `text` and Return into a terminal agent (as a paste when it takes bracketed pastes,
    /// so a multi-line text stays one message), or sends it as a chat agent's prompt.
    pub(super) fn send_text(&self, text: &str, cx: &mut App) -> anyhow::Result<()> {
        match &self.surface {
            AgentSurface::Terminal { handle, .. } => {
                let bracketed = handle.session().terminal().modes().bracketed_paste;
                let mut bytes = if bracketed {
                    hangar_terminal::encode_paste(text, true)
                } else {
                    text.as_bytes().to_vec()
                };
                bytes.push(b'\r');
                handle.session().write(bytes)?;
                Ok(())
            }
            AgentSurface::Chat { view, .. } => view.update(cx, |chat, cx| chat.send(text, cx)),
        }
    }

    /// Types `text` into a terminal agent, or into a chat agent's composer, without sending.
    pub(super) fn prefill(
        &self,
        text: &str,
        window: &mut Window,
        cx: &mut App,
    ) -> anyhow::Result<()> {
        match &self.surface {
            AgentSurface::Terminal { handle, .. } => {
                handle.session().write(text.to_owned())?;
                Ok(())
            }
            AgentSurface::Chat { view, .. } => {
                view.update(cx, |chat, cx| chat.prefill(text, window, cx));
                Ok(())
            }
        }
    }

    pub(super) fn exit_waiter(&self) -> ExitWaiter {
        match &self.surface {
            AgentSurface::Terminal { handle, .. } => {
                ExitWaiter::Terminal(Arc::clone(handle.session()))
            }
            AgentSurface::Chat { acp, .. } => ExitWaiter::Acp(Arc::clone(acp)),
        }
    }
}

/// Waits off the main thread for a stopped agent's process to go away.
pub(super) enum ExitWaiter {
    Terminal(Arc<hangar_terminal::TerminalSession>),
    Acp(Arc<AcpSession>),
}

impl ExitWaiter {
    /// Whether the process ended within `timeout`. Blocks.
    pub(super) fn wait(&self, timeout: Duration) -> bool {
        match self {
            Self::Terminal(terminal) => terminal.wait_for_exit(timeout).is_some(),
            Self::Acp(acp) => acp.wait_for_exit(timeout),
        }
    }
}

pub(super) struct DiffEntry {
    pub(super) diff: Entity<WorkspaceDiff>,
    pub(super) _observe: Subscription,
}

/// Drag payload of the sidebar's resize handle.
pub(super) struct SidebarResize;

impl Render for SidebarResize {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        Empty
    }
}

/// Drag payload of the right panel's resize handle.
pub(super) struct PanelResize;

impl Render for PanelResize {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        Empty
    }
}

/// What fills the window between the title bar and the status bar.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Screen {
    /// Sidebar, panes and the right panel.
    Workspace,
    /// Full-width diff review of the focused workspace (⇧⌘R).
    Review,
    /// Full-width file browser of the focused workspace (⇧⌘E).
    Files,
    /// Every workspace as a card, grouped by state (⇧⌘O).
    Overview,
    /// Settings (⌘,).
    Settings,
}
