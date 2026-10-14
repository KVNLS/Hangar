//! "Notify when an agent needs me": the Dock badge counts workspaces waiting on approval, input
//! or a failure. While Hangar is in the background the Dock bounces when one starts waiting, and a
//! system notification says which workspace waits or finished its turn.

use super::*;
use crate::notify;
pub(super) use hangar_usecases::agents::Attention;

impl HangarApp {
    /// Brings the Dock badge up to date; while no Hangar window is key, bounces the Dock for new
    /// waits and posts notifications. AppKit is only called when something changed.
    pub(super) fn sync_attention(&mut self, cx: &mut Context<Self>) {
        let states: Vec<(WorkspaceId, AgentState)> = self
            .work
            .projects
            .entries()
            .iter()
            .flat_map(|e| e.active())
            .map(|ws| (ws.id, self.agent_state(ws.id, cx)))
            .collect();
        let observed = self.live.attention.observe(states);
        if let Some(count) = observed.badge {
            tracing::info!(count, "Dock badge");
        }
        if let Some(count) = observed.badge
            && let Err(err) = crate::dock::set_badge(&crate::dock::badge_label(count))
        {
            tracing::warn!("could not set the Dock badge: {err:#}");
        }
        if cx.active_window().is_some() {
            return;
        }
        let waits = if self.live.attention.enabled {
            observed.waiting
        } else {
            Vec::new()
        };
        if !waits.is_empty()
            && let Err(err) = crate::dock::request_attention()
        {
            tracing::warn!("could not request the user's attention: {err:#}");
        }
        let events = waits
            .into_iter()
            .map(|(id, state)| (id, notify::Event::Waiting(state)))
            .chain(
                observed
                    .finished
                    .into_iter()
                    .map(|(id, state)| (id, notify::Event::Finished(state))),
            );
        let banners: Vec<notify::Banner> = events
            .filter_map(|(id, event)| {
                let ws = self.work.projects.workspace(id)?;
                let project = self.work.projects.project(ws.project_id)?;
                Some(notify::banner_for(&ws.name, &project.name, event))
            })
            .collect();
        if banners.is_empty() {
            return;
        }
        cx.background_executor()
            .spawn(async move {
                for banner in notify::coalesce(banners) {
                    if let Err(err) = notify::post(&banner) {
                        tracing::warn!("could not post a notification: {err:#}");
                    }
                }
            })
            .detach();
    }

    pub(super) fn set_notify_finished(
        &mut self,
        on: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.live.attention.notify_finished = on;
        self.persist(window, cx, move |store| {
            ops::save_notify_finished(store, on)
        });
        cx.notify();
    }

    pub(super) fn set_notify_attention(
        &mut self,
        on: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.live.attention.enabled = on;
        self.sync_attention(cx);
        self.persist(window, cx, move |store| {
            ops::save_notify_attention(store, on)
        });
        cx.notify();
    }
}
