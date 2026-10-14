//! "Max concurrent agents" as a pure policy: whether a start may run now or waits its turn, and
//! which queued workspace starts when a slot frees. The caller counts live agents; this type
//! only holds the limit and the FIFO queue of starts with what each was asked to run.

use std::collections::VecDeque;
use std::ops::RangeInclusive;

use hangar_core::WorkspaceId;

/// The limit until the user picks one.
pub const DEFAULT_MAX_AGENTS: u32 = 8;
/// Values Settings accepts.
pub const MAX_AGENTS_RANGE: RangeInclusive<u32> = 1..=32;

/// `n` moved into [`MAX_AGENTS_RANGE`].
pub fn clamp_limit(n: u32) -> u32 {
    n.clamp(*MAX_AGENTS_RANGE.start(), *MAX_AGENTS_RANGE.end())
}

/// What happened to a start request.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Admission {
    /// Run it now.
    Start,
    /// Queued behind `ahead` other workspaces.
    Queued { ahead: usize },
}

/// The limit and the workspaces waiting for a slot, each with its start request `T`.
#[derive(Debug)]
pub struct AgentQueue<T> {
    limit: u32,
    waiting: VecDeque<(WorkspaceId, T)>,
}

impl<T> Default for AgentQueue<T> {
    fn default() -> Self {
        Self::new(DEFAULT_MAX_AGENTS)
    }
}

impl<T> AgentQueue<T> {
    pub fn new(limit: u32) -> Self {
        Self {
            limit: clamp_limit(limit),
            waiting: VecDeque::new(),
        }
    }

    pub fn limit(&self) -> u32 {
        self.limit
    }

    /// Changes the limit for later starts only: running agents are never stopped. The caller
    /// drains with [`Self::next`] afterwards, since a higher limit may free slots.
    pub fn set_limit(&mut self, limit: u32) {
        self.limit = clamp_limit(limit);
    }

    /// Decides a start for `id` while `running` agents are live. It starts only below the limit
    /// and with nobody waiting (earlier requests go first). A workspace already queued keeps its
    /// place and takes the newer request.
    pub fn admit(&mut self, id: WorkspaceId, running: usize, request: T) -> Admission {
        if let Some(index) = self.index(id) {
            self.waiting[index].1 = request;
            return Admission::Queued { ahead: index };
        }
        if running < self.limit as usize && self.waiting.is_empty() {
            return Admission::Start;
        }
        self.waiting.push_back((id, request));
        Admission::Queued {
            ahead: self.waiting.len() - 1,
        }
    }

    /// The next workspace to start now that `running` agents are live, removed from the queue.
    pub fn next(&mut self, running: usize) -> Option<(WorkspaceId, T)> {
        if running < self.limit as usize {
            self.waiting.pop_front()
        } else {
            None
        }
    }

    /// Removes `id` from the queue (cancelled, "Start now", archived or deleted) and returns its
    /// request.
    pub fn remove(&mut self, id: WorkspaceId) -> Option<T> {
        let index = self.index(id)?;
        self.waiting.remove(index).map(|(_, request)| request)
    }

    /// How many workspaces wait before `id`, if it is queued.
    pub fn ahead(&self, id: WorkspaceId) -> Option<usize> {
        self.index(id)
    }

    pub fn len(&self) -> usize {
        self.waiting.len()
    }

    pub fn is_empty(&self) -> bool {
        self.waiting.is_empty()
    }

    fn index(&self, id: WorkspaceId) -> Option<usize> {
        self.waiting.iter().position(|(queued, _)| *queued == id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ids<const N: usize>() -> [WorkspaceId; N] {
        std::array::from_fn(|_| WorkspaceId::new())
    }

    #[test]
    fn starts_below_the_limit_and_queues_at_it() {
        let [a, b] = ids();
        let mut queue = AgentQueue::new(2);
        assert_eq!(queue.admit(a, 1, ()), Admission::Start);
        assert_eq!(queue.admit(a, 2, ()), Admission::Queued { ahead: 0 });
        assert_eq!(queue.admit(b, 2, ()), Admission::Queued { ahead: 1 });
        assert_eq!(queue.len(), 2);
        assert_eq!(queue.next(2), None);
    }

    #[test]
    fn starts_in_fifo_order() {
        let [a, b, c] = ids();
        let mut queue = AgentQueue::new(1);
        for (id, request) in [(a, "a"), (b, "b"), (c, "c")] {
            assert!(matches!(
                queue.admit(id, 1, request),
                Admission::Queued { .. }
            ));
        }
        assert_eq!(queue.ahead(c), Some(2));
        assert_eq!(queue.next(0), Some((a, "a")));
        assert_eq!(queue.next(0), Some((b, "b")));
        assert_eq!(queue.ahead(c), Some(0));
        assert_eq!(queue.next(0), Some((c, "c")));
        assert_eq!(queue.next(0), None);
    }

    #[test]
    fn a_free_slot_goes_to_the_queue_first() {
        let [a, b] = ids();
        let mut queue = AgentQueue::new(1);
        queue.admit(a, 1, ());
        assert_eq!(queue.admit(b, 0, ()), Admission::Queued { ahead: 1 });
    }

    #[test]
    fn requeuing_keeps_the_place_and_takes_the_new_request() {
        let [a, b] = ids();
        let mut queue = AgentQueue::new(1);
        queue.admit(a, 1, "old");
        queue.admit(b, 1, "b");
        assert_eq!(queue.admit(a, 1, "new"), Admission::Queued { ahead: 0 });
        assert_eq!(queue.len(), 2);
        assert_eq!(queue.next(0), Some((a, "new")));
    }

    #[test]
    fn cancel_dequeues() {
        let [a, b, c] = ids();
        let mut queue = AgentQueue::new(1);
        for id in [a, b, c] {
            queue.admit(id, 1, ());
        }
        assert_eq!(queue.remove(b), Some(()));
        assert_eq!(queue.remove(b), None);
        assert_eq!(queue.ahead(b), None);
        assert_eq!(queue.ahead(c), Some(1));
        assert_eq!(queue.next(0).map(|(id, _)| id), Some(a));
        assert_eq!(queue.next(0).map(|(id, _)| id), Some(c));
        assert!(queue.is_empty());
    }

    #[test]
    fn limit_changes_affect_new_starts_only() {
        let [a, b] = ids();
        let mut queue = AgentQueue::new(4);
        // Lowered under the running count: nothing to stop, new starts wait.
        queue.set_limit(2);
        assert_eq!(queue.admit(a, 3, ()), Admission::Queued { ahead: 0 });
        assert_eq!(queue.next(2), None);
        assert_eq!(queue.next(1).map(|(id, _)| id), Some(a));
        // Raised: queued workspaces may start right away.
        queue.admit(b, 2, ());
        queue.set_limit(3);
        assert_eq!(queue.next(2).map(|(id, _)| id), Some(b));
    }

    #[test]
    fn limit_is_clamped() {
        assert_eq!(AgentQueue::<()>::new(0).limit(), 1);
        assert_eq!(AgentQueue::<()>::new(99).limit(), 32);
        assert_eq!(AgentQueue::<()>::default().limit(), DEFAULT_MAX_AGENTS);
        let mut queue = AgentQueue::<()>::new(8);
        queue.set_limit(40);
        assert_eq!(queue.limit(), 32);
    }
}
