//! UI-independent pieces of the changes panel: the per-workspace file map, the grouped file
//! list, selection keeping and the flattened diff rows.

use std::collections::BTreeMap;
use std::ops::Range;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use hangar_worktree::{FileDiff, FileStatus, Hunk, LineKind};

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

/// Sidebar badge: `3 +12 −4`, or `None` when nothing changed.
pub fn badge_text(files: usize, insertions: u64, deletions: u64) -> Option<String> {
    (files > 0).then(|| format!("{files} +{insertions} −{deletions}"))
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
mod tests {
    use super::*;
    use hangar_worktree::DiffLine;

    fn line(kind: LineKind, text: &str) -> DiffLine {
        DiffLine {
            kind,
            old_lineno: None,
            new_lineno: None,
            text: text.into(),
        }
    }

    fn file(path: &str, status: FileStatus, hunks: Vec<Hunk>) -> FileDiff {
        FileDiff {
            path: path.into(),
            old_path: None,
            status,
            old_mode: None,
            new_mode: None,
            binary: false,
            truncated: false,
            insertions: 0,
            deletions: 0,
            hunks,
        }
    }

    fn hunk(lines: Vec<DiffLine>) -> Hunk {
        Hunk {
            old_start: 1,
            old_lines: 2,
            new_start: 1,
            new_lines: 3,
            header: String::new(),
            lines,
        }
    }

    fn map(files: Vec<FileDiff>) -> FileMap {
        files
            .into_iter()
            .map(|f| (f.path.clone(), Arc::new(f)))
            .collect()
    }

    #[test]
    fn apply_update_replaces_and_removes() {
        let mut files = map(vec![
            file("a", FileStatus::Modified, vec![]),
            file("b", FileStatus::Added, vec![]),
        ]);
        let kept = Arc::clone(&files[Path::new("a")]);
        assert!(apply_update(
            &mut files,
            vec![file("c", FileStatus::Untracked, vec![])],
            &["b".into()]
        ));
        assert_eq!(
            files.keys().collect::<Vec<_>>(),
            [Path::new("a"), Path::new("c")]
        );
        // Untouched entries keep their allocation.
        assert!(Arc::ptr_eq(&kept, &files[Path::new("a")]));
        assert!(!apply_update(&mut files, vec![], &["zzz".into()]));
    }

    #[test]
    fn badges_and_shas() {
        assert_eq!(badge_text(0, 0, 0), None);
        assert_eq!(badge_text(3, 12, 4).as_deref(), Some("3 +12 −4"));
        assert_eq!(short_sha("0123456789abcdef"), "0123456");
        assert_eq!(short_sha("abc"), "abc");
    }

    #[test]
    fn file_list_groups_in_order() {
        let files = map(vec![
            file("z.rs", FileStatus::Modified, vec![]),
            file("new.rs", FileStatus::Untracked, vec![]),
            file("gone.rs", FileStatus::Deleted, vec![]),
            file("a.rs", FileStatus::Renamed, vec![]),
            file("conflict.rs", FileStatus::Unmerged, vec![]),
            file("added.rs", FileStatus::Added, vec![]),
        ]);
        let list = file_list(&files);
        let shape: Vec<String> = list
            .iter()
            .map(|e| match e {
                ListEntry::Header { group, count } => format!("# {} {count}", group.label()),
                ListEntry::File(f) => f.path.display().to_string(),
            })
            .collect();
        assert_eq!(
            shape,
            [
                "# Conflicts 1",
                "conflict.rs",
                "# Modified 2",
                "a.rs",
                "z.rs",
                "# Added 2",
                "added.rs",
                "new.rs",
                "# Deleted 1",
                "gone.rs",
            ]
        );
        assert_eq!(list_paths(&list).len(), 6);
        assert!(file_list(&FileMap::new()).is_empty());
    }

    #[test]
    fn reselect_keeps_follows_and_falls_back() {
        let before = map(vec![
            file("a", FileStatus::Modified, vec![]),
            file("b", FileStatus::Modified, vec![]),
            file("c", FileStatus::Modified, vec![]),
        ]);
        let previous = list_paths(&file_list(&before));
        let list = file_list(&before);
        assert_eq!(reselect(&previous, &list, None), Some("a".into()));
        assert_eq!(
            reselect(&previous, &list, Some(Path::new("b"))),
            Some("b".into())
        );

        // "b" gone: the file now at its position.
        let after = map(vec![
            file("a", FileStatus::Modified, vec![]),
            file("c", FileStatus::Modified, vec![]),
        ]);
        let list = file_list(&after);
        assert_eq!(
            reselect(&previous, &list, Some(Path::new("b"))),
            Some("c".into())
        );
        // Last one gone: the new last file.
        assert_eq!(
            reselect(&previous, &list, Some(Path::new("zzz"))),
            Some("a".into())
        );

        // Renamed: follow it.
        let mut renamed = file("d", FileStatus::Renamed, vec![]);
        renamed.old_path = Some("b".into());
        let list = file_list(&map(vec![file("a", FileStatus::Modified, vec![]), renamed]));
        assert_eq!(
            reselect(&previous, &list, Some(Path::new("b"))),
            Some("d".into())
        );

        assert_eq!(reselect(&previous, &[], Some(Path::new("a"))), None);
    }

    #[test]
    fn flatten_rows_and_notices() {
        let f = file(
            "x.rs",
            FileStatus::Modified,
            vec![
                hunk(vec![
                    line(LineKind::Context, "a"),
                    line(LineKind::Added, "b"),
                ]),
                hunk(vec![line(LineKind::Removed, "c")]),
            ],
        );
        assert_eq!(
            flatten(&f),
            [
                DiffRow::Hunk { hunk: 0 },
                DiffRow::Line { hunk: 0, line: 0 },
                DiffRow::Line { hunk: 0, line: 1 },
                DiffRow::Hunk { hunk: 1 },
                DiffRow::Line { hunk: 1, line: 0 },
            ]
        );

        let mut binary = f.clone();
        binary.binary = true;
        assert_eq!(flatten(&binary), [DiffRow::Notice(Notice::Binary)]);

        let mut truncated = f.clone();
        truncated.truncated = true;
        assert_eq!(
            flatten(&truncated).last(),
            Some(&DiffRow::Notice(Notice::Truncated))
        );
        truncated.hunks.clear();
        assert_eq!(flatten(&truncated), [DiffRow::Notice(Notice::Truncated)]);

        let empty = file("m", FileStatus::TypeChanged, vec![]);
        assert_eq!(flatten(&empty), [DiffRow::Notice(Notice::NoContent)]);
    }

    #[test]
    fn widest_row_counts_tabs_and_notices() {
        let mut f = file(
            "x.rs",
            FileStatus::Modified,
            vec![hunk(vec![
                line(LineKind::Context, "0123456789"),
                line(LineKind::Added, "\t\t\t"),
                line(LineKind::Added, "short"),
            ])],
        );
        assert_eq!(widest_row(&f, &flatten(&f)), 0, "hunk header is the widest");
        f.hunks[0].lines[0].text = "x".repeat(40);
        assert_eq!(widest_row(&f, &flatten(&f)), 1);
        f.hunks[0].lines[1].text = "\t".repeat(14);
        assert_eq!(widest_row(&f, &flatten(&f)), 2);
        f.truncated = true;
        f.hunks[0].lines.truncate(1);
        f.hunks[0].lines[0].text = "x".into();
        assert_eq!(widest_row(&f, &flatten(&f)), 2, "truncation notice");
    }

    #[test]
    fn titles_and_hunk_headers() {
        let mut f = file("new/name.rs", FileStatus::Renamed, vec![]);
        assert_eq!(file_title(&f), "new/name.rs");
        f.old_path = Some("old/name.rs".into());
        assert_eq!(file_title(&f), "old/name.rs → new/name.rs");

        let mut h = hunk(vec![]);
        assert_eq!(hunk_header(&h), "@@ -1,2 +1,3 @@");
        h.header = "fn main() {".into();
        assert_eq!(hunk_header(&h), "@@ -1,2 +1,3 @@ fn main() {");
    }

    #[test]
    fn side_sources_split_old_and_new() {
        let f = file(
            "x.rs",
            FileStatus::Modified,
            vec![
                hunk(vec![
                    line(LineKind::Context, "ctx"),
                    line(LineKind::Removed, "old"),
                    line(LineKind::Added, "new"),
                    line(LineKind::NoNewlineAtEof, ""),
                ]),
                hunk(vec![line(LineKind::Added, "two")]),
            ],
        );
        let rows = flatten(&f);
        let sources = side_sources(&f, &rows);
        assert_eq!(sources.old, "ctx\nold\n\n");
        assert_eq!(sources.new, "ctx\nnew\n\ntwo\n");
        assert_eq!(
            sources.spans,
            [
                None,
                Some((Side::New, 0..3)),
                Some((Side::Old, 4..7)),
                Some((Side::New, 4..7)),
                None,
                None,
                Some((Side::New, 9..12)),
            ]
        );
    }

    #[test]
    fn display_lines_expand_tabs_and_cut() {
        let line = DisplayLine::new("\tlet x = 1;");
        assert_eq!(line.text, "    let x = 1;");
        assert_eq!(line.map(0), 0);
        assert_eq!(line.map(1), 4);
        assert_eq!(line.map_range(&(1..4)), Some(4..7));

        let long = "é".repeat(MAX_LINE_BYTES);
        let cut = DisplayLine::new(&long);
        assert!(cut.text.ends_with('…'));
        assert_eq!(cut.text.len(), MAX_LINE_BYTES + '…'.len_utf8());
        assert_eq!(cut.map(MAX_LINE_BYTES * 2), MAX_LINE_BYTES);
        assert_eq!(
            cut.map_range(&(MAX_LINE_BYTES + 2..MAX_LINE_BYTES + 4)),
            None
        );
    }

    #[test]
    fn split_runs_per_row() {
        // Side text "ab\n\tc\n": row 1 is "\tc" at 3..5.
        let runs = vec![(0..2, 'k'), (2..4, 's'), (4..5, 'v')];
        let row0 = DisplayLine::new("ab");
        assert_eq!(split_runs(&runs, &(0..2), &row0), [(0..2, 'k')]);
        let row1 = DisplayLine::new("\tc");
        assert_eq!(
            split_runs(&runs, &(3..5), &row1),
            [(0..4, 's'), (4..5, 'v')]
        );
        assert!(split_runs(&runs, &(6..6), &DisplayLine::new("")).is_empty());
    }
}
