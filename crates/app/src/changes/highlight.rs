//! Syntax highlighting of diff rows with gpui-component's tree-sitter `SyntaxHighlighter`.
//! Runs on a background thread: each side of the file is parsed once, then the highlight runs
//! are split per row.

use std::ops::Range;
use std::sync::Arc;

use gpui_kit::HighlightStyle;
use gpui_kit::base::input::Rope;
use gpui_kit::component::highlighter::{HighlightTheme, SyntaxHighlighter};
use hangar_worktree::FileDiff;

use super::model::{DiffRow, DisplayLine, Side, side_sources, split_runs};

/// Highlight runs of one row, in its display text's byte offsets.
pub type RowStyles = Vec<(Range<usize>, HighlightStyle)>;

/// Per-row styles (`None` for rows without code: hunk headers, notices).
pub fn highlight_rows(
    file: &FileDiff,
    rows: &[DiffRow],
    language: &str,
    theme: &HighlightTheme,
) -> Vec<Option<RowStyles>> {
    let sources = side_sources(file, rows);
    let old = side_runs(&sources.old, language, theme);
    let new = side_runs(&sources.new, language, theme);
    rows.iter()
        .zip(&sources.spans)
        .map(|(row, span)| {
            let (side, range) = span.as_ref()?;
            let DiffRow::Line { hunk, line } = *row else {
                return None;
            };
            let text = &file.hunks.get(hunk)?.lines.get(line)?.text;
            let runs = match side {
                Side::Old => &old,
                Side::New => &new,
            };
            Some(split_runs(runs, range, &DisplayLine::new(text)))
        })
        .collect()
}

/// Styled runs over a whole side, without unstyled gaps.
fn side_runs(
    text: &str,
    language: &str,
    theme: &HighlightTheme,
) -> Vec<(Range<usize>, HighlightStyle)> {
    if text.is_empty() {
        return Vec::new();
    }
    let mut highlighter = SyntaxHighlighter::new(language);
    let rope = Rope::from_str(text);
    highlighter.update(None, &rope, None);
    highlighter
        .styles(&(0..text.len()), theme)
        .into_iter()
        .filter(|(range, style)| !range.is_empty() && *style != HighlightStyle::default())
        .collect()
}

/// Theme handle shared with the background thread.
pub type Theme = Arc<HighlightTheme>;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::changes::model::flatten;
    use hangar_worktree::{DiffLine, FileStatus, Hunk, LineKind};

    #[test]
    fn rust_rows_get_styles_from_both_sides() {
        let lines = vec![
            (LineKind::Context, "fn main() {"),
            (LineKind::Removed, "    let x = \"old\";"),
            (LineKind::Added, "\tlet x = \"new\";"),
            (LineKind::Context, "}"),
        ];
        let file = FileDiff {
            path: "main.rs".into(),
            old_path: None,
            status: FileStatus::Modified,
            old_mode: None,
            new_mode: None,
            binary: false,
            truncated: false,
            insertions: 1,
            deletions: 1,
            hunks: vec![Hunk {
                old_start: 1,
                old_lines: 3,
                new_start: 1,
                new_lines: 3,
                header: String::new(),
                lines: lines
                    .into_iter()
                    .map(|(kind, text)| DiffLine {
                        kind,
                        old_lineno: None,
                        new_lineno: None,
                        text: text.into(),
                    })
                    .collect(),
            }],
        };
        let rows = flatten(&file);
        let theme = HighlightTheme::default_dark();
        let styles = highlight_rows(&file, &rows, "rust", &theme);
        assert_eq!(styles.len(), rows.len());
        assert!(styles[0].is_none(), "hunk header");
        let keyword = styles[1].as_ref().expect("context line styled");
        assert!(
            keyword.iter().any(|(r, _)| *r == (0..2)),
            "`fn` keyword: {keyword:?}"
        );
        let removed = styles[2].as_ref().expect("removed line styled");
        assert!(
            removed.iter().any(|(r, _)| *r == (12..17)),
            "string: {removed:?}"
        );
        // Added line starts with a tab: offsets are in display text (4 spaces).
        let added = styles[3].as_ref().expect("added line styled");
        assert!(
            added.iter().any(|(r, _)| *r == (12..17)),
            "string: {added:?}"
        );

        // Unknown language: no styles, no panic.
        let plain = highlight_rows(&file, &rows, "text", &theme);
        assert!(plain.iter().flatten().all(|s| s.is_empty()));
    }
}
