//! "Notify when an agent needs me": the Dock badge counts workspaces waiting on approval, input
//! or a failure, and the Dock bounces when one starts waiting while Hangar is in the background.

use super::*;
pub(super) use hangar_usecases::agents::Attention;

impl HangarApp {
    /// Brings the Dock badge up to date and bounces the Dock for new waits while no Hangar
    /// window is key. AppKit is only called when something changed.
    pub(super) fn sync_attention(&mut self, cx: &mut Context<Self>) {
        let states: Vec<(WorkspaceId, AgentState)> = self
            .work
            .projects
            .entries()
            .iter()
            .flat_map(|e| e.active())
            .map(|ws| (ws.id, self.agent_state(ws.id, cx)))
            .collect();
        let observed = self.attention.observe(states);
        if let Some(count) = observed.badge {
            tracing::info!(count, "Dock badge");
        }
        if let Some(count) = observed.badge
            && let Err(err) = crate::dock::set_badge(&crate::dock::badge_label(count))
        {
            tracing::warn!("could not set the Dock badge: {err:#}");
        }
        if self.attention.enabled
            && observed.newly_waiting > 0
            && cx.active_window().is_none()
            && let Err(err) = crate::dock::request_attention()
        {
            tracing::warn!("could not request the user's attention: {err:#}");
        }
    }

    pub(super) fn set_notify_attention(
        &mut self,
        on: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.attention.enabled = on;
        self.sync_attention(cx);
        self.persist(window, cx, move |store| {
            ops::save_notify_attention(store, on)
        });
        cx.notify();
    }
}
