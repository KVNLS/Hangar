//! GPUI-free state of the logs pane: a capped line buffer with level and text filters, the
//! follow-tail decision and the copy selection.

use std::collections::VecDeque;
use std::ops::RangeInclusive;

use hangar_devices::{LogEntry, LogLevel};

/// Entries kept by the view; older ones are discarded.
pub const BUFFER_CAP: usize = 20_000;
/// Longest stored line, in characters. Longer ones are cut so shaping stays cheap.
const MAX_LINE_CHARS: usize = 1_000;

/// One formatted log line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Line {
    pub seq: u64,
    pub level: LogLevel,
    pub text: String,
}

/// `HH:MM:SS.mmm L source[pid] message`.
pub fn format_entry(entry: &LogEntry) -> String {
    let time = entry
        .timestamp
        .split_whitespace()
        .nth(1)
        .and_then(|t| t.split(['+', '-']).next())
        .map(|t| t.chars().take(12).collect::<String>())
        .unwrap_or_default();
    let source = if entry.process.is_empty() {
        entry.tag.as_str()
    } else {
        entry.process.as_str()
    };
    let mut text = format!("{time} {} {source}", level_letter(entry.level));
    if let Some(pid) = entry.pid {
        text.push_str(&format!("[{pid}]"));
    }
    text.push(' ');
    text.push_str(&entry.message);
    sanitize(&text)
}

fn sanitize(text: &str) -> String {
    text.chars()
        .take(MAX_LINE_CHARS)
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect()
}

pub fn level_letter(level: LogLevel) -> char {
    match level {
        LogLevel::Verbose => 'V',
        LogLevel::Debug => 'D',
        LogLevel::Info => 'I',
        LogLevel::Warn => 'W',
        LogLevel::Error => 'E',
        LogLevel::Fault => 'F',
    }
}

#[derive(Debug)]
pub struct LogModel {
    lines: VecDeque<Line>,
    /// Sequence numbers of the lines passing the filters, ascending. A line's position in
    /// `lines` is `seq - lines[0].seq`.
    visible: VecDeque<u64>,
    cap: usize,
    next_seq: u64,
    min_level: LogLevel,
    /// Lowercased search text; empty matches everything.
    query: String,
    /// The visible line with the most characters (sequence number, length): the list is as
    /// wide as it, so long lines scroll sideways.
    widest: Option<(u64, usize)>,
    /// Warnings and errors (faults included) held, filtered or not.
    counts: LevelCounts,
}

/// Warnings and errors among the held lines, kept as lines come and go.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct LevelCounts {
    pub warnings: usize,
    pub errors: usize,
}

impl LevelCounts {
    fn slot(&mut self, level: LogLevel) -> Option<&mut usize> {
        match level {
            LogLevel::Warn => Some(&mut self.warnings),
            LogLevel::Error | LogLevel::Fault => Some(&mut self.errors),
            _ => None,
        }
    }

    fn add(&mut self, level: LogLevel) {
        if let Some(n) = self.slot(level) {
            *n += 1;
        }
    }

    fn remove(&mut self, level: LogLevel) {
        if let Some(n) = self.slot(level) {
            *n = n.saturating_sub(1);
        }
    }

    /// `2 warnings · 1 error`.
    pub fn summary(self) -> String {
        let plural = |n: usize, what: &str| format!("{n} {what}{}", if n == 1 { "" } else { "s" });
        format!(
            "{} · {}",
            plural(self.warnings, "warning"),
            plural(self.errors, "error")
        )
    }
}

impl LogModel {
    pub fn new(cap: usize) -> Self {
        Self {
            lines: VecDeque::new(),
            visible: VecDeque::new(),
            cap: cap.max(1),
            next_seq: 0,
            min_level: LogLevel::Verbose,
            query: String::new(),
            widest: None,
            counts: LevelCounts::default(),
        }
    }

    fn passes(&self, level: LogLevel, text: &str) -> bool {
        level >= self.min_level
            && (self.query.is_empty() || text.to_lowercase().contains(&self.query))
    }

    /// Appends entries, discarding the oldest lines beyond the cap. Returns how many of the
    /// new lines are visible.
    pub fn push(&mut self, entries: impl IntoIterator<Item = LogEntry>) -> usize {
        let mut shown = 0;
        for entry in entries {
            let line = Line {
                seq: self.next_seq,
                level: entry.level,
                text: format_entry(&entry),
            };
            self.next_seq += 1;
            let mut evicted_widest = false;
            if self.lines.len() >= self.cap
                && let Some(old) = self.lines.pop_front()
            {
                self.counts.remove(old.level);
                if self.visible.front() == Some(&old.seq) {
                    self.visible.pop_front();
                    evicted_widest = self.widest.is_some_and(|(seq, _)| seq == old.seq);
                }
            }
            self.counts.add(line.level);
            if self.passes(line.level, &line.text) {
                self.visible.push_back(line.seq);
                shown += 1;
                let chars = line.text.chars().count();
                if self.widest.is_none_or(|(_, widest)| chars > widest) {
                    self.widest = Some((line.seq, chars));
                }
            }
            self.lines.push_back(line);
            if evicted_widest {
                self.measure_widest();
            }
        }
        shown
    }

    /// Position (among visible lines) of the widest one.
    pub fn widest_index(&self) -> Option<usize> {
        let (seq, _) = self.widest?;
        self.visible.binary_search(&seq).ok()
    }

    fn measure_widest(&mut self) {
        self.widest = None;
        for &seq in &self.visible {
            let first = self.lines.front().map_or(seq, |l| l.seq);
            let Some(line) = usize::try_from(seq - first)
                .ok()
                .and_then(|i| self.lines.get(i))
            else {
                continue;
            };
            let chars = line.text.chars().count();
            if self.widest.is_none_or(|(_, widest)| chars > widest) {
                self.widest = Some((seq, chars));
            }
        }
    }

    pub fn clear(&mut self) {
        self.lines.clear();
        self.visible.clear();
        self.widest = None;
        self.counts = LevelCounts::default();
    }

    pub fn counts(&self) -> LevelCounts {
        self.counts
    }

    pub fn set_min_level(&mut self, level: LogLevel) {
        if level != self.min_level {
            self.min_level = level;
            self.refilter();
        }
    }

    pub fn set_query(&mut self, query: &str) {
        let query = query.trim().to_lowercase();
        if query != self.query {
            self.query = query;
            self.refilter();
        }
    }

    fn refilter(&mut self) {
        self.visible = self
            .lines
            .iter()
            .filter(|l| self.passes(l.level, &l.text))
            .map(|l| l.seq)
            .collect();
        self.measure_widest();
    }

    /// Lines passing the filters.
    pub fn len(&self) -> usize {
        self.visible.len()
    }

    /// Lines held, filtered or not.
    pub fn total(&self) -> usize {
        self.lines.len()
    }

    pub fn get(&self, ix: usize) -> Option<&Line> {
        let seq = *self.visible.get(ix)?;
        let first = self.lines.front()?.seq;
        self.lines.get(usize::try_from(seq - first).ok()?)
    }

    /// The visible lines whose sequence number is in `range` (`None`: all), joined by newlines.
    pub fn text(&self, range: Option<&RangeInclusive<u64>>) -> String {
        let mut out = String::new();
        for ix in 0..self.visible.len() {
            let Some(line) = self.get(ix) else { continue };
            if range.is_some_and(|r| !r.contains(&line.seq)) {
                continue;
            }
            if !out.is_empty() {
                out.push('\n');
            }
            out.push_str(&line.text);
        }
        out
    }
}

/// Whether the list sticks to the newest line.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Follow(bool);

impl Default for Follow {
    fn default() -> Self {
        Self(true)
    }
}

impl Follow {
    pub fn is_on(self) -> bool {
        self.0
    }

    /// The user scrolled; `at_end` is where the list ended up (`None`: it does not scroll).
    pub fn user_scrolled(&mut self, at_end: Option<bool>) {
        self.0 = at_end.unwrap_or(true);
    }

    pub fn set(&mut self, on: bool) {
        self.0 = on;
    }
}

/// A click on line `seq` selects it, or extends the selection from `anchor` with shift.
pub fn select(anchor: Option<u64>, seq: u64, shift: bool) -> (u64, RangeInclusive<u64>) {
    match anchor {
        Some(a) if shift => (a, a.min(seq)..=a.max(seq)),
        _ => (seq, seq..=seq),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(level: LogLevel, msg: &str) -> LogEntry {
        LogEntry::new(level, msg)
    }

    fn texts(m: &LogModel) -> Vec<String> {
        (0..m.len())
            .filter_map(|i| m.get(i).map(|l| l.text.clone()))
            .collect()
    }

    #[test]
    fn ring_buffer_discards_oldest() {
        let mut m = LogModel::new(3);
        m.push((0..5).map(|i| entry(LogLevel::Info, &format!("m{i}"))));
        assert_eq!(m.total(), 3);
        assert_eq!(m.len(), 3);
        assert!(m.get(0).is_some_and(|l| l.text.ends_with("m2")));
        assert!(m.get(2).is_some_and(|l| l.text.ends_with("m4")));
        assert!(m.get(3).is_none());
    }

    #[test]
    fn widest_line_follows_pushes_eviction_and_filters() {
        let mut m = LogModel::new(3);
        assert_eq!(m.widest_index(), None);
        m.push([
            entry(LogLevel::Info, "a very long line indeed"),
            entry(LogLevel::Error, "short"),
            entry(LogLevel::Info, "medium line"),
        ]);
        assert_eq!(m.widest_index(), Some(0));
        // The widest line is evicted: the next widest takes over.
        m.push([entry(LogLevel::Info, "x")]);
        let widest = m
            .widest_index()
            .and_then(|i| m.get(i))
            .map(|l| l.text.clone());
        assert!(widest.is_some_and(|t| t.ends_with("medium line")));
        // Only visible lines count.
        m.set_min_level(LogLevel::Error);
        let widest = m
            .widest_index()
            .and_then(|i| m.get(i))
            .map(|l| l.text.clone());
        assert!(widest.is_some_and(|t| t.ends_with("short")));
        m.clear();
        assert_eq!(m.widest_index(), None);
    }

    #[test]
    fn level_and_query_filter_with_eviction() {
        let mut m = LogModel::new(4);
        m.push([
            entry(LogLevel::Debug, "alpha"),
            entry(LogLevel::Error, "Beta"),
            entry(LogLevel::Info, "gamma"),
            entry(LogLevel::Error, "delta"),
        ]);
        m.set_min_level(LogLevel::Warn);
        assert_eq!(m.len(), 2);
        m.set_query(" BETA ");
        assert_eq!(m.len(), 1);
        m.set_query("");
        // Evicts "alpha" (not visible), then "Beta" (visible).
        m.push([entry(LogLevel::Fault, "eps")]);
        m.push([entry(LogLevel::Warn, "zeta")]);
        assert_eq!(m.total(), 4);
        let t = texts(&m);
        assert_eq!(t.len(), 3);
        assert!(t[0].ends_with("delta") && t[2].ends_with("zeta"));
        m.set_min_level(LogLevel::Verbose);
        assert_eq!(m.len(), 4);
    }

    #[test]
    fn push_reports_visible_count_and_clear_empties() {
        let mut m = LogModel::new(10);
        m.set_min_level(LogLevel::Error);
        assert_eq!(
            m.push([entry(LogLevel::Info, "a"), entry(LogLevel::Error, "b")]),
            1
        );
        m.clear();
        assert_eq!((m.len(), m.total()), (0, 0));
    }

    #[test]
    fn warning_and_error_counts_follow_the_ring_buffer() {
        let mut m = LogModel::new(3);
        assert_eq!(m.counts(), LevelCounts::default());
        m.push([
            entry(LogLevel::Warn, "w1"),
            entry(LogLevel::Error, "e1"),
            entry(LogLevel::Info, "i1"),
        ]);
        assert_eq!(
            m.counts(),
            LevelCounts {
                warnings: 1,
                errors: 1
            }
        );
        // Filters do not change the counts.
        m.set_min_level(LogLevel::Fault);
        m.set_query("nothing matches");
        // Evicts the warning, then the error; a fault counts as an error.
        m.push([entry(LogLevel::Fault, "f1"), entry(LogLevel::Debug, "d1")]);
        assert_eq!(
            m.counts(),
            LevelCounts {
                warnings: 0,
                errors: 1
            }
        );
        assert_eq!(m.counts().summary(), "0 warnings · 1 error");
        m.push([entry(LogLevel::Warn, "w2"), entry(LogLevel::Warn, "w3")]);
        assert_eq!(m.counts().summary(), "2 warnings · 0 errors");
        m.clear();
        assert_eq!(m.counts(), LevelCounts::default());
    }

    #[test]
    fn follow_tracks_where_the_user_scrolled() {
        let mut f = Follow::default();
        assert!(f.is_on());
        f.user_scrolled(Some(false));
        assert!(!f.is_on());
        f.user_scrolled(Some(true));
        assert!(f.is_on());
        f.user_scrolled(Some(false));
        f.user_scrolled(None);
        assert!(f.is_on());
    }

    #[test]
    fn selection_and_copy() {
        assert_eq!(select(None, 5, false), (5, 5..=5));
        assert_eq!(select(Some(2), 5, true), (2, 2..=5));
        assert_eq!(select(Some(7), 5, true), (7, 5..=7));
        assert_eq!(select(Some(2), 5, false), (5, 5..=5));
        let mut m = LogModel::new(10);
        m.push((0..4).map(|i| entry(LogLevel::Info, &format!("m{i}"))));
        assert_eq!(m.text(None).lines().count(), 4);
        let part = m.text(Some(&(1..=2)));
        assert_eq!(part.lines().count(), 2);
        assert!(part.lines().next().is_some_and(|l| l.ends_with("m1")));
    }

    #[test]
    fn formats_android_and_ios_entries() {
        let mut e = entry(LogLevel::Warn, "boom");
        e.timestamp = "10-04 12:34:56.789".into();
        e.tag = "ActivityManager".into();
        e.pid = Some(42);
        assert_eq!(format_entry(&e), "12:34:56.789 W ActivityManager[42] boom");
        e.timestamp = "2026-10-04 12:34:56.123456+0200".into();
        e.process = "SpringBoard".into();
        assert!(format_entry(&e).starts_with("12:34:56.123 W SpringBoard[42] "));
        e.message = "a\tb".into();
        assert!(format_entry(&e).ends_with("a b"));
    }
}
