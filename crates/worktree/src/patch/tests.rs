use super::*;

fn parse(out: &[u8]) -> Vec<FileDiff> {
    parse_patch(out, &DiffOptions::default()).unwrap()
}

/// Real `git diff --raw -p -z` output covering modes, binary, CRLF, renames, missing
/// newlines, spaces and quoted names.
const MIXED: &[u8] = b":100755 100644 4cb29ea 0000000 M\0a.txt\0\
:100644 100644 88768ef 0000000 M\0b.bin\0\
:100644 100644 0aa0f34 0000000 M\0crlf.txt\0\
:100644 100644 0fdf397 0fdf397 R100\0mv.txt\0moved.txt\0\
:100644 100644 1a9d148 0000000 M\0nonl.txt\0\
:100644 100644 b478595 0000000 M\0sp ace.txt\0\
:000000 100644 0000000 bca70f3 A\0tab\tx.txt\0\
:000000 100644 0000000 c600332 A\0\xc3\xa9.txt\0\
:100644 000000 e69de29 0000000 D\0empty\0\
\0diff --git a/a.txt b/a.txt
old mode 100755
new mode 100644
index 4cb29ea..ddc897f
--- a/a.txt
+++ b/a.txt
@@ -1,3 +1,3 @@ fn main
 one
-two
+TWO
 three
diff --git a/b.bin b/b.bin
index 88768ef..3e3315e 100644
Binary files a/b.bin and b/b.bin differ
diff --git a/crlf.txt b/crlf.txt
index 0aa0f34..a092d71 100644
--- a/crlf.txt
+++ b/crlf.txt
@@ -1,2 +1,2 @@
 x\r
-y\r
+Y\r
diff --git a/mv.txt b/moved.txt
similarity index 100%
rename from mv.txt
rename to moved.txt
diff --git a/nonl.txt b/nonl.txt
index 1a9d148..7adf2e5 100644
--- a/nonl.txt
+++ b/nonl.txt
@@ -1 +1 @@
-nonl
\\ No newline at end of file
+nonl2
\\ No newline at end of file
diff --git a/sp ace.txt b/sp ace.txt
index b478595..4ae8ef0 100644
--- a/sp ace.txt\t
+++ b/sp ace.txt\t
@@ -1 +1 @@
-s
+u
diff --git \"a/tab\\tx.txt\" \"b/tab\\tx.txt\"
new file mode 100644
index 0000000..bca70f3
--- /dev/null
+++ \"b/tab\\tx.txt\"
@@ -0,0 +1 @@
+q
diff --git \"a/\\303\\251.txt\" \"b/\\303\\251.txt\"
new file mode 100644
index 0000000..c600332
--- /dev/null
+++ \"b/\\303\\251.txt\"
@@ -0,0 +1 @@
+\xc3\xa9
diff --git a/empty b/empty
deleted file mode 100644
index e69de29..0000000
";

fn find<'a>(files: &'a [FileDiff], path: &str) -> &'a FileDiff {
    files
        .iter()
        .find(|f| f.path == Path::new(path))
        .unwrap_or_else(|| panic!("{path} missing from {files:#?}"))
}

use std::path::Path;

#[test]
fn parses_mixed_patch() {
    let files = parse(MIXED);
    assert_eq!(files.len(), 9);

    let a = find(&files, "a.txt");
    assert_eq!(a.status, FileStatus::Modified);
    assert_eq!((a.old_mode, a.new_mode), (Some(0o100755), Some(0o100644)));
    assert_eq!((a.insertions, a.deletions), (1, 1));
    let hunk = &a.hunks[0];
    assert_eq!(
        (
            hunk.old_start,
            hunk.old_lines,
            hunk.new_start,
            hunk.new_lines
        ),
        (1, 3, 1, 3)
    );
    assert_eq!(hunk.header, "fn main");
    let kinds: Vec<_> = hunk
        .lines
        .iter()
        .map(|l| (l.kind, l.old_lineno, l.new_lineno, l.text.as_str()))
        .collect();
    assert_eq!(
        kinds,
        vec![
            (LineKind::Context, Some(1), Some(1), "one"),
            (LineKind::Removed, Some(2), None, "two"),
            (LineKind::Added, None, Some(2), "TWO"),
            (LineKind::Context, Some(3), Some(3), "three"),
        ]
    );

    let bin = find(&files, "b.bin");
    assert!(bin.binary && bin.hunks.is_empty());

    let crlf = find(&files, "crlf.txt");
    assert_eq!(crlf.hunks[0].lines[2].text, "Y");

    let moved = find(&files, "moved.txt");
    assert_eq!(moved.status, FileStatus::Renamed);
    assert_eq!(moved.old_path.as_deref(), Some(Path::new("mv.txt")));
    assert!(moved.hunks.is_empty());

    let nonl = find(&files, "nonl.txt");
    let lines = &nonl.hunks[0].lines;
    assert_eq!(lines.len(), 4);
    assert_eq!(lines[1].kind, LineKind::NoNewlineAtEof);
    assert_eq!(lines[1].text, "No newline at end of file");
    assert_eq!(lines[3].kind, LineKind::NoNewlineAtEof);

    assert_eq!(find(&files, "sp ace.txt").hunks[0].lines[1].text, "u");
    let tab = find(&files, "tab\tx.txt");
    assert_eq!(tab.status, FileStatus::Added);
    assert_eq!(tab.new_mode, Some(0o100644));
    assert_eq!(tab.old_mode, None);
    assert_eq!(find(&files, "é.txt").hunks[0].lines[0].text, "é");

    let empty = find(&files, "empty");
    assert_eq!(empty.status, FileStatus::Deleted);
    assert!(empty.hunks.is_empty());
}

#[test]
fn parses_empty_output() {
    assert!(parse(b"").is_empty());
}

#[test]
fn patch_without_raw_section_uses_headers() {
    let out = b"diff --git a/old name.rs b/new name.rs
similarity index 90%
rename from old name.rs
rename to new name.rs
index 1..2 100644
--- a/old name.rs
+++ b/new name.rs
@@ -1,2 +1,2 @@
 keep
-a
+b
diff --git a/gone.rs b/gone.rs
deleted file mode 100755
index 1..0
--- a/gone.rs
+++ /dev/null
@@ -1 +0,0 @@
-bye
";
    let files = parse(out);
    assert_eq!(files.len(), 2);
    let gone = find(&files, "gone.rs");
    assert_eq!(gone.status, FileStatus::Deleted);
    assert_eq!(gone.old_mode, Some(0o100755));
    assert_eq!(gone.hunks[0].lines[0].old_lineno, Some(1));
    let renamed = find(&files, "new name.rs");
    assert_eq!(renamed.status, FileStatus::Renamed);
    assert_eq!(renamed.old_path.as_deref(), Some(Path::new("old name.rs")));
    assert_eq!((renamed.insertions, renamed.deletions), (1, 1));
}

#[test]
fn merges_type_change_sections() {
    let out = b":100644 120000 1 2 T\0link\0\0diff --git a/link b/link
deleted file mode 100644
index 1..0
--- a/link
+++ /dev/null
@@ -1 +0,0 @@
-text
diff --git a/link b/link
new file mode 120000
index 0..2
--- /dev/null
+++ b/link
@@ -0,0 +1 @@
+target
\\ No newline at end of file
";
    let files = parse(out);
    assert_eq!(files.len(), 1);
    let link = &files[0];
    assert_eq!(link.status, FileStatus::TypeChanged);
    assert_eq!(
        (link.old_mode, link.new_mode),
        (Some(0o100644), Some(0o120000))
    );
    assert_eq!(link.hunks.len(), 2);
    assert_eq!((link.insertions, link.deletions), (1, 1));
}

#[test]
fn unmerged_entry_without_patch() {
    let out = b":000000 000000 0 0 U\0conflict.rs\0\0* Unmerged path conflict.rs\n";
    let files = parse(out);
    assert_eq!(files.len(), 1);
    assert_eq!(files[0].status, FileStatus::Unmerged);
    assert_eq!((files[0].old_mode, files[0].new_mode), (None, None));
}

#[test]
fn copy_and_multiple_hunks() {
    let out = b":100644 100644 1 1 C75\0src.rs\0dst.rs\0\0diff --git a/src.rs b/dst.rs
similarity index 75%
copy from src.rs
copy to dst.rs
--- a/src.rs
+++ b/dst.rs
@@ -1,2 +1,2 @@
-a
+A
 b
@@ -10,2 +10,3 @@ impl X
 j

+k
";
    let files = parse(out);
    let dst = &files[0];
    assert_eq!(dst.status, FileStatus::Copied);
    assert_eq!(dst.old_path.as_deref(), Some(Path::new("src.rs")));
    assert_eq!(dst.hunks.len(), 2);
    let second = &dst.hunks[1];
    assert_eq!(
        (second.old_start, second.old_lines, second.new_lines),
        (10, 2, 3)
    );
    assert_eq!(second.header, "impl X");
    // A blank line inside a hunk (diff.suppressBlankEmpty) is context.
    assert_eq!(second.lines[1].kind, LineKind::Context);
    assert_eq!(second.lines[1].text, "");
    assert_eq!(second.lines[2].new_lineno, Some(12));
}

#[test]
fn truncates_long_files_but_counts_everything() {
    let mut out = b"diff --git a/big b/big\n--- a/big\n+++ b/big\n@@ -0,0 +1,50 @@\n".to_vec();
    for i in 0..50 {
        out.extend_from_slice(format!("+line {i}\n").as_bytes());
    }
    out.extend_from_slice(
        b"diff --git a/small b/small\n--- a/small\n+++ b/small\n@@ -1 +1 @@\n-x\n+y\n",
    );
    let opts = DiffOptions {
        max_lines_per_file: Some(10),
        ..DiffOptions::default()
    };
    let files = parse_patch(&out, &opts).unwrap();
    let big = &files[0];
    assert!(big.truncated);
    assert_eq!(big.line_count(), 10);
    assert_eq!(big.insertions, 50);
    let small = &files[1];
    assert!(!small.truncated);
    assert_eq!(small.line_count(), 2);
}

#[test]
fn header_path_containing_b_slash() {
    let out = b"diff --git a/x b/y b/x b/y\n--- a/x b/y\n+++ b/x b/y\n@@ -1 +1 @@\n-1\n+2\n";
    let files = parse(out);
    assert_eq!(files[0].path, Path::new("x b/y"));
}

#[test]
fn mixed_quoting_in_header() {
    let (a, b) = split_header_paths(b"a/plain \"b/tab\\there\"").unwrap();
    assert_eq!(a, Path::new("plain"));
    assert_eq!(b, Path::new("tab\there"));
    let (a, b) = split_header_paths(b"\"a/q\\\"uote\" b/plain").unwrap();
    assert_eq!(a, Path::new("q\"uote"));
    assert_eq!(b, Path::new("plain"));
}

#[test]
fn rejects_malformed_patches() {
    let short = b"diff --git a/f b/f\n--- a/f\n+++ b/f\n@@ -1,3 +1,3 @@\n a\n";
    assert!(parse_patch(short, &DiffOptions::default()).is_err());
    let long = b"diff --git a/f b/f\n@@ -1 +1 @@\n-a\n+b\n+c\n";
    assert!(parse_patch(long, &DiffOptions::default()).is_err());
    let header = b"diff --git a/f b/f\n@@ -x +1 @@\n";
    assert!(parse_patch(header, &DiffOptions::default()).is_err());
    assert!(
        parse_patch(
            b":100644 100644 1 2 M\0unterminated",
            &DiffOptions::default()
        )
        .is_err()
    );
    assert!(take_quoted(b"\"open").is_err());
    assert!(take_quoted(b"\"x\\4\"").is_err());
}
