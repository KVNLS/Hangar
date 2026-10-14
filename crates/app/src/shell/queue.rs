//! "Max concurrent agents": starts beyond the limit wait as Idle in FIFO order and start when an
//! agent stops. The policy is [`hangar_usecases::agents::AgentQueue`]; this wires it to the sessions.

use gpui_kit::assets::IconName as Lucide;

use super::toast::ToastKind;
use super::*;
use crate::ui::components::{EmptyState, UiButton};
pub(super) use hangar_usecases::agents::StartRequest;
use hangar_usecases::agents::{Admission, DEFAULT_MAX_AGENTS, MAX_AGENTS_RANGE, clamp_limit};

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
                self.save_agent_queue(window, cx);
                let limit = self.live.agent_queue.limit();
                self.show_toast(
                    ToastKind::Info,
                    format!(
                        "“{name}” queued: {}. It starts when one stops.",
                        agents_at_once(limit)
                    ),
                    cx,
                );
                Task::ready(false)
            }
        }
    }

    /// Starts queued workspaces while slots are free, oldest first, then saves what still
    /// waits. Waits for the login PATH, without which agents may not be found.
    pub(super) fn drain_agent_queue(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.agent_setup.path.is_none() {
            self.save_agent_queue(window, cx);
            return;
        }
        while let Some((id, request)) = self.live.agent_queue.next(self.running_agents()) {
            if !self.work.projects.is_active(id) || self.work.busy.contains(&id) {
                continue;
            }
            tracing::info!(workspace = %id, "starting queued agent");
            self.launch_session(id, request, window, cx).detach();
        }
        self.save_agent_queue(window, cx);
        cx.notify();
    }

    /// Puts back the starts that waited when Hangar quit (workspaces still active only) and
    /// starts those that fit.
    pub(super) fn restore_agent_queue(
        &mut self,
        saved: Vec<(WorkspaceId, StartRequest)>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.live.saved_queue = saved.clone();
        let saved: Vec<_> = saved
            .into_iter()
            .filter(|(id, _)| self.work.projects.is_active(*id))
            .collect();
        if !saved.is_empty() {
            tracing::info!(count = saved.len(), "restoring queued agent starts");
        }
        self.live.agent_queue.restore(saved);
        self.drain_agent_queue(window, cx);
    }

    /// Saves the queue when it differs from the last saved one.
    fn save_agent_queue(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let queue: Vec<(WorkspaceId, StartRequest)> = self
            .live
            .agent_queue
            .waiting()
            .map(|(id, request)| (id, request.clone()))
            .collect();
        if queue == self.live.saved_queue {
            return;
        }
        self.live.saved_queue = queue.clone();
        self.persist(window, cx, move |store| {
            ops::save_agent_queue(store, &queue)
        });
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
        self.save_agent_queue(window, cx);
        cx.notify();
    }

    fn cancel_queued(&mut self, id: WorkspaceId, window: &mut Window, cx: &mut Context<Self>) {
        self.live.agent_queue.remove(id);
        self.save_agent_queue(window, cx);
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
        EmptyState::new(Lucide::Hourglass, queued_label(ahead))
            .hint(format!(
                "{} (Settings › Agents & providers).",
                agents_at_once(self.max_agents())
            ))
            .action(
                UiButton::new(SharedString::from(format!("queue-start-{id}")))
                    .icon(Lucide::Play)
                    .label("Start now")
                    .small()
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.start_queued_now(id, window, cx)
                    })),
            )
            .action(
                UiButton::new(SharedString::from(format!("queue-cancel-{id}")))
                    .label("Cancel")
                    .ghost()
                    .small()
                    .on_click(
                        cx.listener(move |this, _, window, cx| this.cancel_queued(id, window, cx)),
                    ),
            )
    }
}

/// "Queued · next · …" for the head of the queue, else "Queued · N ahead · …".
pub(super) fn queued_label(ahead: usize) -> String {
    match ahead {
        0 => "Queued · next · starts when an agent stops".to_owned(),
        n => format!("Queued · {n} ahead · starts when an agent stops"),
    }
}

/// "1 agent at once" / "N agents at once".
fn agents_at_once(limit: u32) -> String {
    let s = if limit == 1 { "" } else { "s" };
    format!("{limit} agent{s} at once")
}

#[cfg(test)]
mod copy_tests {
    use super::{agents_at_once, queued_label};

    #[test]
    fn queue_copy_reads_naturally() {
        assert_eq!(
            queued_label(0),
            "Queued · next · starts when an agent stops"
        );
        assert_eq!(
            queued_label(2),
            "Queued · 2 ahead · starts when an agent stops"
        );
        assert_eq!(agents_at_once(1), "1 agent at once");
        assert_eq!(agents_at_once(8), "8 agents at once");
    }
}
