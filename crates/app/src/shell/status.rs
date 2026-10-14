//! Per-workspace agent state for the sidebar, status bar and overview. PTY agents report only
//! running/exited, so "working" is inferred from recent terminal output.

use std::time::Instant;

use super::*;

/// Output within this window counts as working. Agent TUIs animate while thinking and go quiet
/// while waiting for the user.
const ACTIVE_WINDOW: Duration = Duration::from_secs(3);
const ACTIVITY_TICK: Duration = Duration::from_secs(1);

#[derive(Clone, Debug, Default)]
pub(super) struct Activity {
    bytes: u64,
    last_output: Option<Instant>,
    active: bool,
    /// Last meaningful line on the terminal screen, refreshed when output arrives.
    pub last_line: Option<SharedString>,
}

/// Characters agent TUIs draw boxes and prompts with; trimmed from screen lines.
fn is_decoration(c: char) -> bool {
    c.is_whitespace()
        || matches!(
            c,
            '\u{2500}'..='\u{259F}' | '>' | '›' | '❯' | '$' | '%' | '#' | '·' | '•' | '*'
        )
}

/// The last screen row (bottom-up) that says something: decoration trimmed, at least two
/// alphanumeric characters.
pub(super) fn summary_line<I: DoubleEndedIterator<Item = String>>(rows: I) -> Option<String> {
    rows.rev().find_map(|row| {
        let text = row.trim_matches(is_decoration).trim();
        (text.chars().filter(|c| c.is_alphanumeric()).count() >= 2).then(|| text.to_owned())
    })
}

/// The workspace after `current` among those waiting on the user (approval, input, failed,
/// review), in that order then sidebar order; wraps around. `items` is in sidebar order.
pub(super) fn next_needing_you<T: Copy + PartialEq>(
    items: &[(AgentState, T)],
    current: Option<T>,
) -> Option<T> {
    let mut queue: Vec<(AgentState, T)> = items
        .iter()
        .copied()
        .filter(|(state, _)| state.needs_you() || *state == AgentState::Review)
        .collect();
    queue.sort_by_key(|(state, _)| *state);
    let at = current.and_then(|c| queue.iter().position(|(_, id)| *id == c));
    let next = at.map_or(0, |at| (at + 1) % queue.len().max(1));
    queue.get(next).map(|(_, id)| *id)
}

/// `now`, `42s`, `11m`, `3h`, `2d` since `then_ms` (Unix milliseconds).
pub(super) fn age_label(then_ms: i64, now_ms: i64) -> String {
    let secs = (now_ms - then_ms).max(0) / 1000;
    match secs {
        0..=9 => "now".to_owned(),
        10..=59 => format!("{secs}s"),
        60..=3599 => format!("{}m", secs / 60),
        3600..=86_399 => format!("{}h", secs / 3600),
        _ => format!("{}d", secs / 86_400),
    }
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
                let snapshot = live.view.read(cx).session().terminal().snapshot();
                activity.last_line = summary_line((0..snapshot.rows).map(|r| snapshot.row_text(r)))
                    .map(SharedString::from);
                changed = true;
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

    /// Focuses the next workspace waiting on the user (⌘J).
    pub(super) fn focus_next_needing_you(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let items: Vec<(AgentState, WorkspaceId)> = self
            .projects
            .entries()
            .iter()
            .flat_map(|e| e.active())
            .map(|ws| (self.agent_state(ws.id, cx), ws.id))
            .collect();
        let current = self.panes.slot(self.panes.focused());
        match next_needing_you(&items, current) {
            Some(id) => {
                self.set_screen(Screen::Workspace, window, cx);
                self.select_workspace(id, window, cx);
            }
            None => {
                window.push_notification("No workspace is waiting on you.", cx);
            }
        }
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

#[cfg(test)]
mod tests {
    // Not `super::*`: the shell glob-imports gpui, whose `test` macro would shadow `#[test]`.
    use super::{age_label, next_needing_you, summary_line};
    use hangar_core::AgentState::*;

    fn rows(lines: &[&str]) -> std::vec::IntoIter<String> {
        lines
            .iter()
            .map(|l| (*l).to_owned())
            .collect::<Vec<_>>()
            .into_iter()
    }

    #[test]
    fn summary_skips_boxes_prompts_and_blanks() {
        let screen = rows(&[
            "⏺ Editing OrderListScreen.kt",
            "  LazyColumn with sticky headers",
            "╭──────────────────────╮",
            "│ >                    │",
            "╰──────────────────────╯",
            "",
        ]);
        assert_eq!(
            summary_line(screen).as_deref(),
            Some("LazyColumn with sticky headers")
        );
        assert_eq!(summary_line(rows(&["$ ", "", "│ │"])), None);
        assert_eq!(
            summary_line(rows(&["❯ npm test"])).as_deref(),
            Some("npm test")
        );
    }

    #[test]
    fn next_needing_you_cycles_in_priority_order() {
        let items = [
            (Working, 1),
            (Review, 2),
            (Failed, 3),
            (Idle, 4),
            (Review, 5),
        ];
        assert_eq!(next_needing_you(&items, None), Some(3));
        assert_eq!(next_needing_you(&items, Some(3)), Some(2));
        assert_eq!(next_needing_you(&items, Some(2)), Some(5));
        assert_eq!(next_needing_you(&items, Some(5)), Some(3));
        // From a workspace that is not waiting: the first in line.
        assert_eq!(next_needing_you(&items, Some(1)), Some(3));
        assert_eq!(next_needing_you(&[(Working, 1), (Idle, 2)], Some(1)), None);
    }

    #[test]
    fn ages() {
        assert_eq!(age_label(0, 5_000), "now");
        assert_eq!(age_label(0, 42_000), "42s");
        assert_eq!(age_label(0, 11 * 60_000), "11m");
        assert_eq!(age_label(0, 3 * 3_600_000), "3h");
        assert_eq!(age_label(0, 49 * 3_600_000), "2d");
        assert_eq!(age_label(10_000, 0), "now");
    }
}
