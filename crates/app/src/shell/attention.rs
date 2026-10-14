//! "Notify when an agent needs me": the Dock badge counts workspaces waiting on approval, input
//! or a failure, and the Dock bounces when one starts waiting while Hangar is in the background.

use std::hash::Hash;

use super::*;

/// What the Dock shows, and each workspace's state as last seen to spot new waits.
pub(super) struct Attention<K> {
    pub enabled: bool,
    states: HashMap<K, AgentState>,
    /// The count on the Dock badge (0: none).
    shown: usize,
}

/// The outcome of one [`Attention::observe`].
#[derive(Debug, PartialEq, Eq)]
pub(super) struct Observed {
    /// The badge count to set, when it differs from the one shown.
    pub badge: Option<usize>,
    /// Workspaces that started waiting on the user since the last observation.
    pub newly_waiting: usize,
}

impl<K: Copy + Eq + Hash> Attention<K> {
    pub(super) fn new(enabled: bool) -> Self {
        Self {
            enabled,
            states: HashMap::new(),
            shown: 0,
        }
    }

    /// Records the current state of every workspace. Workspaces left out are gone. New waits
    /// are counted whether or not the toggle is on, so turning it on does not replay old ones.
    pub(super) fn observe(
        &mut self,
        current: impl IntoIterator<Item = (K, AgentState)>,
    ) -> Observed {
        let current: HashMap<K, AgentState> = current.into_iter().collect();
        let newly_waiting = current
            .iter()
            .filter(|(id, state)| {
                state.needs_you() && !self.states.get(id).is_some_and(|s| s.needs_you())
            })
            .count();
        let waiting = current.values().filter(|s| s.needs_you()).count();
        self.states = current;
        let wanted = if self.enabled { waiting } else { 0 };
        let badge = (wanted != self.shown).then(|| {
            self.shown = wanted;
            wanted
        });
        Observed {
            badge,
            newly_waiting,
        }
    }
}

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
}

#[cfg(test)]
mod tests {
    // Not `super::*`: the shell glob-imports gpui, whose `test` macro would shadow `#[test]`.
    use super::{Attention, Observed};
    use hangar_core::AgentState::*;

    fn seen(badge: Option<usize>, newly_waiting: usize) -> Observed {
        Observed {
            badge,
            newly_waiting,
        }
    }

    #[test]
    fn counts_approval_input_and_failed() {
        let mut a = Attention::new(true);
        let all = [
            (1, Approval),
            (2, Input),
            (3, Failed),
            (4, Review),
            (5, Working),
            (6, Idle),
        ];
        assert_eq!(a.observe(all), seen(Some(3), 3));
        // Same states: nothing to set, nothing new.
        assert_eq!(a.observe(all), seen(None, 0));
    }

    #[test]
    fn only_new_waits_count() {
        let mut a = Attention::new(true);
        assert_eq!(a.observe([(1, Working), (2, Approval)]), seen(Some(1), 1));
        // 2 moves between waiting states: still the same wait.
        assert_eq!(a.observe([(1, Working), (2, Failed)]), seen(None, 0));
        assert_eq!(a.observe([(1, Input), (2, Failed)]), seen(Some(2), 1));
        // Back to work and waiting again is a new wait.
        assert_eq!(a.observe([(1, Working), (2, Failed)]), seen(Some(1), 0));
        assert_eq!(a.observe([(1, Approval), (2, Failed)]), seen(Some(2), 1));
        // A removed workspace drops out of the count.
        assert_eq!(a.observe([(1, Approval)]), seen(Some(1), 0));
        assert_eq!(a.observe([]), seen(Some(0), 0));
    }

    #[test]
    fn badge_only_while_enabled() {
        let mut a = Attention::new(false);
        assert_eq!(a.observe([(1, Approval)]), seen(None, 1));
        a.enabled = true;
        assert_eq!(a.observe([(1, Approval)]), seen(Some(1), 0));
        a.enabled = false;
        assert_eq!(a.observe([(1, Approval)]), seen(Some(0), 0));
        assert_eq!(a.observe([(1, Approval), (2, Failed)]), seen(None, 1));
    }
}
