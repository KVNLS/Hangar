//! "Max concurrent agents": starts beyond the limit wait as Idle in FIFO order and start when an
//! agent stops. The policy is [`crate::agent_queue`]; this wires it to the sessions.

use gpui_kit::assets::IconName as Lucide;

use super::toast::ToastKind;
use super::*;
use crate::agent_queue::Admission;
use crate::ui::components::{EmptyState, UiButton};

/// What to start in a workspace, kept while it waits for a slot.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct StartRequest {
    pub agent: String,
    /// Continue the saved structured conversation (see [`HangarApp::resumable_chat`]).
    pub resume: bool,
    /// Typed into the agent once it runs (the New workspace sheet's task, `HANGAR_DEV_INPUT`).
    pub input: Option<String>,
}

impl HangarApp {
    /// Live agents that hold a slot: sessions not yet ended, plus those still spawning.
    pub(super) fn running_agents(&self) -> usize {
        let live = self
            .live
            .sessions
            .iter()
            .filter(|(id, l)| !l.session.status.is_terminal() && !self.live.starting.contains(id))
            .count();
        live + self.live.starting.len()
    }

    /// Starts `request` in the workspace now, or queues it when the limit is reached.
    pub(super) fn request_agent(
        &mut self,
        id: WorkspaceId,
        request: StartRequest,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Task<bool> {
        if self.live.starting.contains(&id) {
            return Task::ready(false);
        }
        let running = self.running_agents();
        match self.live.agent_queue.admit(id, running, request.clone()) {
            Admission::Start => self.launch_session(id, request, window, cx),
            Admission::Queued { ahead } => {
                let name = self
                    .work
                    .projects
                    .workspace(id)
                    .map(|ws| ws.name.clone())
                    .unwrap_or_default();
                tracing::info!(workspace = %id, ahead, running, "agent start queued");
                let limit = self.live.agent_queue.limit();
                self.show_toast(
                    ToastKind::Info,
                    format!("“{name}” queued: {limit} agents at once. It starts when one stops."),
                    cx,
                );
                Task::ready(false)
            }
        }
    }

    /// Starts queued workspaces while slots are free, oldest first.
    pub(super) fn drain_agent_queue(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        while let Some((id, request)) = self.live.agent_queue.next(self.running_agents()) {
            if !self.work.projects.is_active(id) || self.work.busy.contains(&id) {
                continue;
            }
            tracing::info!(workspace = %id, "starting queued agent");
            self.launch_session(id, request, window, cx).detach();
        }
        cx.notify();
    }

    /// How many workspaces wait before `id`, if it is queued.
    pub(super) fn queued_ahead(&self, id: WorkspaceId) -> Option<usize> {
        self.live.agent_queue.ahead(id)
    }

    pub(super) fn queued_count(&self) -> usize {
        self.live.agent_queue.len()
    }

    /// "Start now": runs a queued workspace past the limit.
    fn start_queued_now(&mut self, id: WorkspaceId, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(request) = self.live.agent_queue.remove(id) {
            self.launch_session(id, request, window, cx).detach();
        }
        cx.notify();
    }

    fn cancel_queued(&mut self, id: WorkspaceId, cx: &mut Context<Self>) {
        self.live.agent_queue.remove(id);
        cx.notify();
    }

    pub(super) fn max_agents(&self) -> u32 {
        self.live.agent_queue.limit()
    }

    /// The pane of a queued workspace: its place and a way to skip or leave the queue.
    pub(super) fn render_queued_pane(
        &self,
        id: WorkspaceId,
        ahead: usize,
        cx: &mut Context<Self>,
    ) -> EmptyState {
        EmptyState::new(
            Lucide::Hourglass,
            format!("Queued · {ahead} ahead · starts when an agent stops"),
        )
        .hint(format!(
            "{} agents at once (Settings › Agents & providers).",
            self.max_agents()
        ))
        .action(
            UiButton::new(SharedString::from(format!("queue-start-{id}")))
                .icon(Lucide::Play)
                .label("Start now")
                .small()
                .on_click(
                    cx.listener(move |this, _, window, cx| this.start_queued_now(id, window, cx)),
                ),
        )
        .action(
            UiButton::new(SharedString::from(format!("queue-cancel-{id}")))
                .label("Cancel")
                .ghost()
                .small()
                .on_click(cx.listener(move |this, _, _, cx| this.cancel_queued(id, cx))),
        )
    }
}
