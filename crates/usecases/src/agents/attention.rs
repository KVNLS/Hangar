//! "Notify when an agent needs me" as a pure policy: which workspaces wait on the user, the
//! count to show (only while enabled), and which started waiting or finished a turn since the
//! last look.

use std::collections::HashMap;
use std::hash::Hash;

use hangar_core::AgentState;

/// What the Dock shows, and each workspace's state as last seen to spot new waits.
pub struct Attention<K> {
    pub enabled: bool,
    /// Report finished turns ("Notify when an agent finishes").
    pub notify_finished: bool,
    states: HashMap<K, AgentState>,
    /// The count on the Dock badge (0: none).
    shown: usize,
}

/// The outcome of one [`Attention::observe`].
#[derive(Debug, PartialEq, Eq)]
pub struct Observed<K> {
    /// The badge count to set, when it differs from the one shown.
    pub badge: Option<usize>,
    /// Workspaces that started waiting on the user since the last observation, in input order.
    pub waiting: Vec<(K, AgentState)>,
    /// Workspaces whose agent stopped working (now ready for review or idle), in input order;
    /// empty unless `notify_finished`.
    pub finished: Vec<(K, AgentState)>,
}

impl<K: Copy + Eq + Hash> Attention<K> {
    pub fn new(enabled: bool) -> Self {
        Self {
            enabled,
            notify_finished: true,
            states: HashMap::new(),
            shown: 0,
        }
    }

    /// Records the current state of every workspace. Workspaces left out are gone. New waits
    /// are counted whether or not the toggle is on, so turning it on does not replay old ones.
    /// A workspace seen for the first time never counts as finished.
    pub fn observe(&mut self, current: impl IntoIterator<Item = (K, AgentState)>) -> Observed<K> {
        let current: Vec<(K, AgentState)> = current.into_iter().collect();
        let waiting = current
            .iter()
            .filter(|(id, state)| {
                state.needs_you() && !self.states.get(id).is_some_and(|s| s.needs_you())
            })
            .copied()
            .collect();
        let finished = current
            .iter()
            .filter(|(id, state)| {
                self.notify_finished
                    && matches!(state, AgentState::Review | AgentState::Idle)
                    && self.states.get(id) == Some(&AgentState::Working)
            })
            .copied()
            .collect();
        let count = current.iter().filter(|(_, s)| s.needs_you()).count();
        self.states = current.into_iter().collect();
        let wanted = if self.enabled { count } else { 0 };
        let badge = (wanted != self.shown).then(|| {
            self.shown = wanted;
            wanted
        });
        Observed {
            badge,
            waiting,
            finished,
        }
    }
}

#[cfg(test)]
mod tests;
