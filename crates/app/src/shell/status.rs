//! Per-workspace agent state for the sidebar, status bar and overview. PTY agents report only
//! running/exited, so "working" is inferred from recent terminal output.

use std::time::Instant;

use super::*;

/// Output within this window counts as working. Agent TUIs animate while thinking and go quiet
/// while waiting for the user.
const ACTIVE_WINDOW: Duration = Duration::from_secs(3);
const ACTIVITY_TICK: Duration = Duration::from_secs(1);

#[derive(Clone, Copy, Debug, Default)]
pub(super) struct Activity {
    bytes: u64,
    last_output: Option<Instant>,
    active: bool,
}

/// Changed files, insertions and deletions of a workspace's live diff.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(super) struct DiffCounts {
    pub files: usize,
    pub insertions: usize,
    pub deletions: usize,
}

impl HangarApp {
    /// Polls terminal output counters and re-renders when a session starts or stops being active.
    pub(super) fn track_activity(cx: &mut Context<Self>) -> Task<()> {
        cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(ACTIVITY_TICK).await;
                let alive = this.update(cx, |this, cx| {
                    if this.tick_activity(cx) {
                        cx.notify();
                    }
                });
                if alive.is_err() {
                    break;
                }
            }
        })
    }

    fn tick_activity(&mut self, cx: &mut Context<Self>) -> bool {
        let now = Instant::now();
        let mut changed = false;
        for live in self.sessions.values_mut() {
            let bytes = live.view.read(cx).session().stats().bytes_read();
            let activity = &mut live.activity;
            if bytes != activity.bytes {
                activity.bytes = bytes;
                activity.last_output = Some(now);
            }
            let active = activity
                .last_output
                .is_some_and(|at| now.duration_since(at) < ACTIVE_WINDOW);
            changed |= active != activity.active;
            activity.active = active;
        }
        changed
    }

    pub(super) fn diff_counts(&self, id: WorkspaceId, cx: &App) -> Option<DiffCounts> {
        let summary = self.diffs.get(&id)?.diff.read(cx).summary()?.clone();
        Some(DiffCounts {
            files: summary.files,
            insertions: usize::try_from(summary.insertions).unwrap_or(usize::MAX),
            deletions: usize::try_from(summary.deletions).unwrap_or(usize::MAX),
        })
    }

    pub(super) fn agent_state(&self, id: WorkspaceId, cx: &App) -> AgentState {
        let live = self.sessions.get(&id);
        let has_changes = self.diff_counts(id, cx).is_some_and(|c| c.files > 0);
        AgentState::from_pty(
            live.map(|l| &l.session.status),
            live.is_some_and(|l| l.activity.active),
            has_changes,
        )
    }

    /// Workspace count per state, in "needs you" order.
    pub(super) fn state_counts(&self, cx: &App) -> Vec<(AgentState, usize)> {
        let mut counts: HashMap<AgentState, usize> = HashMap::new();
        for entry in self.projects.entries() {
            for ws in entry.active() {
                *counts.entry(self.agent_state(ws.id, cx)).or_default() += 1;
            }
        }
        AgentState::ORDER
            .into_iter()
            .filter_map(|s| counts.get(&s).map(|n| (s, *n)))
            .collect()
    }
}
