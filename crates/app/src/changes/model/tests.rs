use super::*;
use hangar_core::changes::DiffLine;

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

#[test]
fn split_rows_pair_removed_with_added() {
    use LineKind::*;
    let f = file(
        "a",
        FileStatus::Modified,
        vec![hunk(vec![
            line(Context, "a"),
            line(Removed, "b"),
            line(Removed, "c"),
            line(Added, "B"),
            line(Context, "d"),
            line(Added, "e"),
            line(Added, "f"),
            line(Removed, "g"),
            line(NoNewlineAtEof, ""),
        ])],
    );
    let rows = flatten(&f);
    let pair = |l: Option<usize>, r: Option<usize>| SplitRow::Pair { left: l, right: r };
    // Row 0 is the hunk header; lines start at 1.
    assert_eq!(
        split_rows(&rows, &f),
        [
            SplitRow::Full(0),
            pair(Some(1), Some(1)),
            pair(Some(2), Some(4)),
            pair(Some(3), None),
            pair(Some(5), Some(5)),
            pair(None, Some(6)),
            pair(None, Some(7)),
            pair(Some(8), None),
            SplitRow::Full(9),
        ]
    );
}
