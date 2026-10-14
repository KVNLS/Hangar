//! Per-workspace agent state for the sidebar, status bar and overview. PTY agents report only
//! running/exited, so "working" is inferred from recent terminal output; structured (ACP) agents
//! report their turn and permission requests.

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
/// [`summary_line`] of a terminal screen, leaving out the cursor's row when it is a bare shell
/// prompt (`kevin@mac ~/app %`): the shell is waiting, and the line above says what happened.
pub(super) fn screen_summary(rows: Vec<String>, cursor_row: Option<usize>) -> Option<String> {
    let waiting = cursor_row.filter(|&r| {
        rows.get(r).is_some_and(|row| {
            row.trim_end()
                .ends_with(['$', '%', '#', '>', '❯', '»', '➜'])
        })
    });
    summary_line(
        rows.into_iter()
            .enumerate()
            .filter(|(i, _)| Some(*i) != waiting)
            .map(|(_, row)| row)
            .collect::<Vec<_>>()
            .into_iter(),
    )
}

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

impl DiffCounts {
    /// As recorded with a finished session.
    pub fn stat(self) -> hangar_core::changes::DiffStat {
        let n = |v: usize| u32::try_from(v).unwrap_or(u32::MAX);
        hangar_core::changes::DiffStat {
            files: n(self.files),
            insertions: n(self.insertions),
            deletions: n(self.deletions),
        }
    }
}

impl HangarApp {
    /// The workspace's diff as recorded when its session ends; `None` when not computed.
    pub(super) fn diff_stat(
        &self,
        id: WorkspaceId,
        cx: &App,
    ) -> Option<hangar_core::changes::DiffStat> {
        self.diff_counts(id, cx).map(DiffCounts::stat)
    }

    /// Polls terminal output counters and re-renders when a session starts or stops being active.
    pub(super) fn track_activity(cx: &mut Context<Self>) -> Task<()> {
        cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(ACTIVITY_TICK).await;
                let alive = this.update(cx, |this, cx| {
                    if this.tick_activity(cx) {
                        cx.notify();
                    }
                    this.sync_attention(cx);
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
        for live in self.live.sessions.values_mut() {
            let view = match &live.surface {
                AgentSurface::Terminal { view, .. } => view,
                AgentSurface::Chat { view, .. } => {
                    // Structured agents report their state; only the summary line is polled.
                    let last_line = view.read(cx).last_line();
                    changed |= last_line != live.activity.last_line;
                    live.activity.last_line = last_line;
                    continue;
                }
            };
            let bytes = view.read(cx).session().stats().bytes_read();
            let activity = &mut live.activity;
            if bytes != activity.bytes {
                activity.bytes = bytes;
                activity.last_output = Some(now);
                let snapshot = view.read(cx).session().terminal().snapshot();
                activity.last_line = screen_summary(
                    (0..snapshot.rows).map(|r| snapshot.row_text(r)).collect(),
                    snapshot.cursor.map(|c| c.row),
                )
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
        let summary = self.live.diffs.get(&id)?.diff.read(cx).summary()?.clone();
        Some(DiffCounts {
            files: summary.files,
            insertions: usize::try_from(summary.insertions).unwrap_or(usize::MAX),
            deletions: usize::try_from(summary.deletions).unwrap_or(usize::MAX),
        })
    }

    pub(super) fn agent_state(&self, id: WorkspaceId, cx: &App) -> AgentState {
        if self.queued_ahead(id).is_some() {
            return AgentState::Idle;
        }
        if self.live.questions.contains_key(&id) {
            return AgentState::Input;
        }
        let live = self.live.sessions.get(&id);
        let has_changes = self.diff_counts(id, cx).is_some_and(|c| c.files > 0);
        match live.map(|l| &l.surface) {
            Some(AgentSurface::Chat { view, .. }) => AgentState::from_acp(
                live.map(|l| &l.session.status),
                view.read(cx).turn(),
                has_changes,
            ),
            _ => match live.and_then(|l| l.signal) {
                Some(signal) => AgentState::from_signal(
                    live.map(|l| &l.session.status),
                    signal,
                    live.is_some_and(|l| l.activity.active),
                    has_changes,
                ),
                None => AgentState::from_pty(
                    live.map(|l| &l.session.status),
                    live.is_some_and(|l| l.activity.active),
                    has_changes,
                ),
            },
        }
    }

    /// Focuses the next workspace waiting on the user (⌘J).
    pub(super) fn focus_next_needing_you(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let items: Vec<(AgentState, WorkspaceId)> = self
            .work
            .projects
            .entries()
            .iter()
            .flat_map(|e| e.active())
            .map(|ws| (self.agent_state(ws.id, cx), ws.id))
            .collect();
        let current = self.layout.panes.slot(self.layout.panes.focused());
        match next_needing_you(&items, current) {
            Some(id) => {
                self.set_screen(Screen::Workspace, window, cx);
                self.select_workspace(id, window, cx);
                self.open_answer_sheet(id, window, cx);
            }
            None => {
                self.show_toast(
                    super::toast::ToastKind::Info,
                    "No workspace is waiting on you.",
                    cx,
                );
            }
        }
    }

    /// Workspace count per state, in "needs you" order.
    pub(super) fn state_counts(&self, cx: &App) -> Vec<(AgentState, usize)> {
        let mut counts: HashMap<AgentState, usize> = HashMap::new();
        for entry in self.work.projects.entries() {
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
    use super::{age_label, next_needing_you, screen_summary, summary_line};
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
    fn screen_summary_skips_a_waiting_prompt() {
        let screen = |lines: &[&str]| lines.iter().map(|l| (*l).to_owned()).collect::<Vec<_>>();
        let done = screen(&["✓ 12 tests passed", "kevin@mac ~/app % ", ""]);
        assert_eq!(
            screen_summary(done.clone(), Some(1)).as_deref(),
            Some("✓ 12 tests passed")
        );
        // Without a cursor on it the prompt row is just the last line.
        assert_eq!(
            screen_summary(done, None).as_deref(),
            Some("kevin@mac ~/app")
        );
        // A typed command is what the user is doing, not a bare prompt.
        let typing = screen(&["build ok", "❯ npm test"]);
        assert_eq!(screen_summary(typing, Some(1)).as_deref(), Some("npm test"));
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
