//! UI-independent pieces of the changes panel: the per-workspace file map, the grouped file
//! list, selection keeping and the flattened diff rows.

use std::collections::BTreeMap;
use std::ops::Range;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use hangar_core::changes::{FileDiff, FileStatus, Hunk, LineKind};

/// Changed files of a workspace by path. `Arc` so a live update only replaces what changed.
pub type FileMap = BTreeMap<PathBuf, Arc<FileDiff>>;

/// Display width of a tab.
const TAB_WIDTH: usize = 4;
/// Longer lines are cut for display: shaping a minified 100 kB line every frame is too slow.
pub const MAX_LINE_BYTES: usize = 2_000;

/// Applies the files of one `DiffUpdate`. Returns whether the map changed.
pub fn apply_update(files: &mut FileMap, changed: Vec<FileDiff>, removed: &[PathBuf]) -> bool {
    let mut dirty = false;
    for path in removed {
        dirty |= files.remove(path).is_some();
    }
    for file in changed {
        files.insert(file.path.clone(), Arc::new(file));
        dirty = true;
    }
    dirty
}

/// Short merge-base for the summary line.
pub fn short_sha(sha: &str) -> &str {
    sha.get(..7).unwrap_or(sha)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Group {
    Conflicted,
    Modified,
    Added,
    Deleted,
}

impl Group {
    pub fn of(status: FileStatus) -> Self {
        match status {
            FileStatus::Unmerged => Self::Conflicted,
            FileStatus::Added | FileStatus::Untracked | FileStatus::Copied => Self::Added,
            FileStatus::Deleted => Self::Deleted,
            FileStatus::Modified | FileStatus::Renamed | FileStatus::TypeChanged => Self::Modified,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Conflicted => "Conflicts",
            Self::Modified => "Modified",
            Self::Added => "Added",
            Self::Deleted => "Deleted",
        }
    }
}

/// One-letter status, as `git status --short` would show it.
pub fn status_letter(status: FileStatus) -> &'static str {
    match status {
        FileStatus::Added => "A",
        FileStatus::Modified => "M",
        FileStatus::Deleted => "D",
        FileStatus::Renamed => "R",
        FileStatus::Copied => "C",
        FileStatus::TypeChanged => "T",
        FileStatus::Untracked => "?",
        FileStatus::Unmerged => "U",
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum ListEntry {
    Header { group: Group, count: usize },
    File(Arc<FileDiff>),
}

/// The file list: conflicts first, then modified, added and deleted files, each sorted by path.
pub fn file_list(files: &FileMap) -> Vec<ListEntry> {
    let mut groups: BTreeMap<Group, Vec<&Arc<FileDiff>>> = BTreeMap::new();
    // The map is sorted by path already.
    for file in files.values() {
        groups.entry(Group::of(file.status)).or_default().push(file);
    }
    let mut list = Vec::with_capacity(files.len() + groups.len());
    for (group, files) in groups {
        list.push(ListEntry::Header {
            group,
            count: files.len(),
        });
        list.extend(files.into_iter().map(|f| ListEntry::File(Arc::clone(f))));
    }
    list
}

/// Paths of the file rows, in list order.
pub fn list_paths(list: &[ListEntry]) -> Vec<PathBuf> {
    list.iter()
        .filter_map(|e| match e {
            ListEntry::File(f) => Some(f.path.clone()),
            ListEntry::Header { .. } => None,
        })
        .collect()
}

/// What stays selected after the list changed: the same path while it is listed, the
/// rename that replaced it, else the file now at its old position (or the last one). With
/// nothing selected before, the first file.
pub fn reselect(
    previous: &[PathBuf],
    list: &[ListEntry],
    selected: Option<&Path>,
) -> Option<PathBuf> {
    let files: Vec<&Arc<FileDiff>> = list
        .iter()
        .filter_map(|e| match e {
            ListEntry::File(f) => Some(f),
            ListEntry::Header { .. } => None,
        })
        .collect();
    let Some(selected) = selected else {
        return files.first().map(|f| f.path.clone());
    };
    if files.iter().any(|f| f.path == selected) {
        return Some(selected.to_path_buf());
    }
    if let Some(renamed) = files
        .iter()
        .find(|f| f.old_path.as_deref() == Some(selected))
    {
        return Some(renamed.path.clone());
    }
    let index = previous.iter().position(|p| p == selected).unwrap_or(0);
    files.get(index).or(files.last()).map(|f| f.path.clone())
}

/// Why a file shows no (or not all) lines.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Notice {
    Binary,
    Truncated,
    /// Mode-only change, conflict without hunks, empty file.
    NoContent,
}

impl Notice {
    pub fn text(self) -> &'static str {
        match self {
            Self::Binary => "Binary file not shown",
            Self::Truncated => "Diff truncated: file too large",
            Self::NoContent => "No textual changes",
        }
    }
}

/// One line of the unified diff view. Lines point into the `FileDiff` so 20k-line diffs do
/// not copy their text.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DiffRow {
    Hunk { hunk: usize },
    Line { hunk: usize, line: usize },
    Notice(Notice),
}

pub fn flatten(file: &FileDiff) -> Vec<DiffRow> {
    if file.binary {
        return vec![DiffRow::Notice(Notice::Binary)];
    }
    let mut rows = Vec::with_capacity(file.line_count() + file.hunks.len() + 1);
    for (h, hunk) in file.hunks.iter().enumerate() {
        rows.push(DiffRow::Hunk { hunk: h });
        rows.extend((0..hunk.lines.len()).map(|line| DiffRow::Line { hunk: h, line }));
    }
    if file.truncated {
        rows.push(DiffRow::Notice(Notice::Truncated));
    } else if rows.is_empty() {
        rows.push(DiffRow::Notice(Notice::NoContent));
    }
    rows
}

/// A row of the side-by-side diff, as indices into the unified rows (so highlighting computed
/// for the unified view is reused).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SplitRow {
    /// Spans both sides: hunk headers, notices, `\ No newline at end of file`.
    Full(usize),
    /// Old side and new side; `None` leaves that side blank.
    Pair {
        left: Option<usize>,
        right: Option<usize>,
    },
}

/// Pairs each run of removed lines with the added lines that follow it, row by row; context
/// lines appear on both sides.
pub fn split_rows(rows: &[DiffRow], file: &FileDiff) -> Vec<SplitRow> {
    fn flush(out: &mut Vec<SplitRow>, removed: &mut Vec<usize>, added: &mut Vec<usize>) {
        let n = removed.len().max(added.len());
        out.extend((0..n).map(|i| SplitRow::Pair {
            left: removed.get(i).copied(),
            right: added.get(i).copied(),
        }));
        removed.clear();
        added.clear();
    }
    let mut out = Vec::with_capacity(rows.len());
    let (mut removed, mut added) = (Vec::new(), Vec::new());
    for (ix, row) in rows.iter().enumerate() {
        let kind = match *row {
            DiffRow::Line { hunk, line } => file
                .hunks
                .get(hunk)
                .and_then(|h| h.lines.get(line))
                .map(|l| l.kind),
            _ => None,
        };
        match kind {
            Some(LineKind::Removed) => {
                if !added.is_empty() {
                    flush(&mut out, &mut removed, &mut added);
                }
                removed.push(ix);
            }
            Some(LineKind::Added) => added.push(ix),
            Some(LineKind::Context) => {
                flush(&mut out, &mut removed, &mut added);
                out.push(SplitRow::Pair {
                    left: Some(ix),
                    right: Some(ix),
                });
            }
            Some(LineKind::NoNewlineAtEof) | None => {
                flush(&mut out, &mut removed, &mut added);
                out.push(SplitRow::Full(ix));
            }
        }
    }
    flush(&mut out, &mut removed, &mut added);
    out
}

pub fn hunk_header(hunk: &Hunk) -> String {
    let mut text = format!(
        "@@ -{},{} +{},{} @@",
        hunk.old_start, hunk.old_lines, hunk.new_start, hunk.new_lines
    );
    if !hunk.header.is_empty() {
        text.push(' ');
        text.push_str(&hunk.header);
    }
    text
}

/// Header of the diff view: `old → new` for renames and copies.
pub fn file_title(file: &FileDiff) -> String {
    match &file.old_path {
        Some(old) if old != &file.path => {
            format!("{} → {}", old.display(), file.path.display())
        }
        _ => file.path.display().to_string(),
    }
}

/// Which reconstructed source a row's text belongs to for highlighting.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Side {
    Old,
    New,
}

/// Old and new sides rebuilt from the hunks (context lines on both), so each side parses as
/// code. `spans[row]` locates the row's text in its side.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct SideSources {
    pub old: String,
    pub new: String,
    pub spans: Vec<Option<(Side, Range<usize>)>>,
}

pub fn side_sources(file: &FileDiff, rows: &[DiffRow]) -> SideSources {
    let mut out = SideSources {
        spans: vec![None; rows.len()],
        ..SideSources::default()
    };
    fn push(side: &mut String, text: &str) -> Range<usize> {
        let start = side.len();
        side.push_str(text);
        side.push('\n');
        start..start + text.len()
    }
    for (index, row) in rows.iter().enumerate() {
        match *row {
            DiffRow::Hunk { hunk } if hunk > 0 => {
                // Keep hunks apart so a construct cut at a hunk edge does not glue to the next.
                out.old.push('\n');
                out.new.push('\n');
            }
            DiffRow::Line { hunk, line } => {
                let Some(line) = file.hunks.get(hunk).and_then(|h| h.lines.get(line)) else {
                    continue;
                };
                out.spans[index] = match line.kind {
                    LineKind::Context => {
                        push(&mut out.old, &line.text);
                        Some((Side::New, push(&mut out.new, &line.text)))
                    }
                    LineKind::Added => Some((Side::New, push(&mut out.new, &line.text))),
                    LineKind::Removed => Some((Side::Old, push(&mut out.old, &line.text))),
                    LineKind::NoNewlineAtEof => None,
                };
            }
            DiffRow::Hunk { .. } | DiffRow::Notice(_) => {}
        }
    }
    out
}

/// A line as drawn: tabs expanded, cut at [`MAX_LINE_BYTES`] on a char boundary.
#[derive(Debug, PartialEq, Eq)]
pub struct DisplayLine {
    pub text: String,
    /// Raw byte offsets of the tabs, in order.
    tabs: Vec<usize>,
    /// Raw length kept.
    kept: usize,
}

impl DisplayLine {
    pub fn new(raw: &str) -> Self {
        let mut kept = raw.len().min(MAX_LINE_BYTES);
        while !raw.is_char_boundary(kept) {
            kept -= 1;
        }
        let raw_kept = &raw[..kept];
        let tabs: Vec<usize> = raw_kept.match_indices('\t').map(|(i, _)| i).collect();
        let mut text = String::with_capacity(kept + tabs.len() * (TAB_WIDTH - 1) + 3);
        for (i, part) in raw_kept.split('\t').enumerate() {
            if i > 0 {
                text.extend(std::iter::repeat_n(' ', TAB_WIDTH));
            }
            text.push_str(part);
        }
        if kept < raw.len() {
            text.push('…');
        }
        Self { text, tabs, kept }
    }

    /// Maps a raw byte offset into `text`. Offsets past the cut land at its end, before `…`.
    pub fn map(&self, raw: usize) -> usize {
        let raw = raw.min(self.kept);
        let tabs_before = self.tabs.partition_point(|&t| t < raw);
        raw + tabs_before * (TAB_WIDTH - 1)
    }

    /// Maps a raw byte range; `None` when nothing of it is visible.
    pub fn map_range(&self, raw: &Range<usize>) -> Option<Range<usize>> {
        let range = self.map(raw.start)..self.map(raw.end);
        (!range.is_empty()).then_some(range)
    }
}

/// Splits highlight runs over a whole side into per-row runs relative to each row's display
/// text. `runs` must be sorted and non-overlapping.
pub fn split_runs<S: Clone>(
    runs: &[(Range<usize>, S)],
    row: &Range<usize>,
    line: &DisplayLine,
) -> Vec<(Range<usize>, S)> {
    let first = runs.partition_point(|(r, _)| r.end <= row.start);
    runs[first..]
        .iter()
        .take_while(|(r, _)| r.start < row.end)
        .filter_map(|(r, style)| {
            let start = r.start.max(row.start) - row.start;
            let end = r.end.min(row.end) - row.start;
            line.map_range(&(start..end)).map(|r| (r, style.clone()))
        })
        .collect()
}

/// Index of the row with the longest text (hunk headers included).
pub fn widest_row(file: &FileDiff, rows: &[DiffRow]) -> usize {
    rows.iter()
        .enumerate()
        .max_by_key(|(_, row)| match **row {
            DiffRow::Line { hunk, line } => file
                .hunks
                .get(hunk)
                .and_then(|h| h.lines.get(line))
                .map_or(0, |l| l.text.len() + l.text.matches('\t').count() * 3),
            DiffRow::Hunk { hunk } => file.hunks.get(hunk).map_or(0, |h| h.header.len() + 24),
            DiffRow::Notice(n) => n.text().len(),
        })
        .map_or(0, |(ix, _)| ix)
}

#[cfg(test)]
mod tests;
