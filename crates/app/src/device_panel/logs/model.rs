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
mod tests;
