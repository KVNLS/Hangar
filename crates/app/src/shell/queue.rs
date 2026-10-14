//! "Max concurrent agents": starts beyond the limit wait as Idle in FIFO order and start when an
//! agent stops. The policy is [`crate::agent_queue`]; this wires it to the sessions.

use gpui_kit::assets::IconName as Lucide;

use super::toast::ToastKind;
use super::*;
use crate::agent_queue::{Admission, DEFAULT_MAX_AGENTS, MAX_AGENTS_RANGE, clamp_limit};
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

    /// Applies a saved or chosen limit. Running agents keep running; a higher one starts
    /// queued workspaces.
    pub(super) fn set_max_agents(
        &mut self,
        limit: u32,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let limit = clamp_limit(limit);
        if limit == self.live.agent_queue.limit() {
            return;
        }
        self.live.agent_queue.set_limit(limit);
        self.persist(window, cx, move |store| ops::save_max_agents(store, limit));
        self.drain_agent_queue(window, cx);
    }

    /// The Settings field for the limit: typed values in range apply at once, anything else
    /// is put back on blur.
    pub(super) fn max_agents_input(
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> (Entity<InputState>, Subscription) {
        let input = cx.new(|cx| {
            InputState::new(window, cx)
                .default_value(DEFAULT_MAX_AGENTS.to_string())
                .min(f64::from(*MAX_AGENTS_RANGE.start()))
                .max(f64::from(*MAX_AGENTS_RANGE.end()))
        });
        let sub = cx.subscribe_in(
            &input,
            window,
            |this, input, event: &InputEvent, window, cx| match event {
                InputEvent::Change => {
                    let typed = input.read(cx).value().trim().parse::<u32>().ok();
                    if let Some(limit) = typed.filter(|n| MAX_AGENTS_RANGE.contains(n)) {
                        this.set_max_agents(limit, window, cx);
                    }
                }
                InputEvent::Blur => this.sync_max_agents_input(window, cx),
                _ => {}
            },
        );
        (input, sub)
    }

    /// Shows the current limit in the Settings field.
    pub(super) fn sync_max_agents_input(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let limit = self.max_agents().to_string();
        let input = self.settings_ui.max_agents.clone();
        if input.read(cx).value() != limit {
            input.update(cx, |input, cx| input.set_value(limit, window, cx));
        }
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
