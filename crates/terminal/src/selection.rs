//! Text selection, kept as an `alacritty_terminal` [`Selection`] inside the `Term` so it
//! follows content as it scrolls into history and is dropped by erase operations. On top of
//! that, a selection is cleared when program output overwrites the selected cells.

use std::hash::{DefaultHasher, Hash, Hasher};

use alacritty_terminal::grid::Dimensions;
use alacritty_terminal::index::{Column, Line, Point, Side as AlacSide};
use alacritty_terminal::selection::{Selection, SelectionRange, SelectionType};
use alacritty_terminal::term::cell::Flags;

use crate::terminal::Terminal;

/// How a selection grows as the pointer moves.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SelectionKind {
    /// Exactly the cells dragged over (single click).
    Simple,
    /// Whole words, separated by the usual semantic delimiters (double click).
    Word,
    /// Whole logical lines, following soft wraps (triple click).
    Line,
    /// The rectangle between the two corners (Option-drag).
    Block,
}

/// Which half of a cell the pointer is over; a selection includes a cell only when it covers
/// the cell past its middle.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Side {
    Left,
    Right,
}

/// A cell in viewport coordinates (row 0 = top visible row). Out-of-range values are clamped.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ViewportPoint {
    pub row: usize,
    pub col: usize,
}

impl ViewportPoint {
    pub fn new(row: usize, col: usize) -> Self {
        Self { row, col }
    }
}

/// The selected cells on the live screen before output was parsed.
pub(crate) struct SelectionFingerprint {
    range: SelectionRange,
    first: Line,
    hash: u64,
}

impl Terminal {
    pub fn start_selection(&mut self, kind: SelectionKind, point: ViewportPoint, side: Side) {
        let ty = match kind {
            SelectionKind::Simple => SelectionType::Simple,
            SelectionKind::Word => SelectionType::Semantic,
            SelectionKind::Line => SelectionType::Lines,
            SelectionKind::Block => SelectionType::Block,
        };
        let point = self.grid_point(point);
        self.term.selection = Some(Selection::new(ty, point, side.into()));
    }

    /// Moves the free end of the selection (drag, or Shift-click to extend). Starts a simple
    /// selection at `point` when there is none.
    pub fn update_selection(&mut self, point: ViewportPoint, side: Side) {
        let grid_point = self.grid_point(point);
        match self.term.selection.as_mut() {
            Some(selection) => selection.update(grid_point, side.into()),
            None => self.start_selection(SelectionKind::Simple, point, side),
        }
    }

    pub fn clear_selection(&mut self) {
        self.term.selection = None;
    }

    /// Whether a non-empty selection exists.
    pub fn has_selection(&self) -> bool {
        self.selection_range().is_some()
    }

    /// Selects the whole scrollback and screen.
    pub fn select_all(&mut self) {
        let start = Point::new(self.term.topmost_line(), Column(0));
        let end = Point::new(self.term.bottommost_line(), self.term.last_column());
        let mut selection = Selection::new(SelectionType::Simple, start, AlacSide::Left);
        selection.update(end, AlacSide::Right);
        self.term.selection = Some(selection);
    }

    /// The selected text: soft-wrapped lines joined, wide glyphs once, trailing blanks of each
    /// line removed. `None` without a selection or when it holds no text.
    pub fn selection_text(&self) -> Option<String> {
        let text = self.term.selection_to_string()?;
        let trimmed = text
            .split('\n')
            .map(|line| line.trim_end_matches(' '))
            .collect::<Vec<_>>()
            .join("\n");
        (!trimmed.trim().is_empty()).then_some(trimmed)
    }

    pub(crate) fn selection_range(&self) -> Option<SelectionRange> {
        self.term.selection.as_ref()?.to_range(&self.term)
    }

    fn grid_point(&self, point: ViewportPoint) -> Point {
        let rows = self.term.screen_lines();
        let cols = self.term.columns();
        let row = point.row.min(rows.saturating_sub(1));
        let col = point.col.min(cols.saturating_sub(1));
        let offset = self.term.grid().display_offset();
        Point::new(Line(row as i32 - offset as i32), Column(col))
    }

    /// Records the selected cells that output could overwrite (those on the live screen).
    pub(crate) fn selection_fingerprint(&self) -> Option<SelectionFingerprint> {
        let range = self.selection_range()?;
        if range.end.line < 0 {
            return None;
        }
        let first = range.start.line.max(Line(0));
        let hash = self.hash_selected(&range, first, 0)?;
        Some(SelectionFingerprint { range, first, hash })
    }

    /// Clears the selection if the cells it covered (after following any scroll) changed.
    pub(crate) fn drop_overwritten_selection(&mut self, before: SelectionFingerprint) {
        let Some(now) = self.selection_range() else {
            return;
        };
        // Scrolling rotates the selection with the text; follow it by the same amount.
        let shift = now.end.line.0 - before.range.end.line.0;
        if self.hash_selected(&before.range, before.first, shift) != Some(before.hash) {
            self.term.selection = None;
        }
    }

    fn hash_selected(&self, range: &SelectionRange, first: Line, shift: i32) -> Option<u64> {
        let grid = self.term.grid();
        let top = self.term.topmost_line();
        let bottom = self.term.bottommost_line();
        let mut hasher = DefaultHasher::new();
        for line in first.0..=range.end.line.0 {
            let target = Line(line + shift);
            if target < top || target > bottom {
                return None;
            }
            let row = &grid[target];
            for col in 0..self.term.columns() {
                if !range.contains(Point::new(Line(line), Column(col))) {
                    continue;
                }
                let cell = &row[Column(col)];
                cell.c.hash(&mut hasher);
                cell.zerowidth().hash(&mut hasher);
                cell.flags
                    .intersects(Flags::WIDE_CHAR | Flags::WIDE_CHAR_SPACER)
                    .hash(&mut hasher);
            }
        }
        Some(hasher.finish())
    }
}

impl From<Side> for AlacSide {
    fn from(side: Side) -> Self {
        match side {
            Side::Left => AlacSide::Left,
            Side::Right => AlacSide::Right,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::snapshot::CellFlags;
    use crate::terminal::{GridSize, TerminalConfig};

    fn term(cols: u16, rows: u16) -> Terminal {
        Terminal::new(
            GridSize::new(cols, rows).unwrap(),
            TerminalConfig::default(),
        )
    }

    fn p(row: usize, col: usize) -> ViewportPoint {
        ViewportPoint::new(row, col)
    }

    fn select(t: &mut Terminal, from: (usize, usize), to: (usize, usize)) {
        t.start_selection(SelectionKind::Simple, p(from.0, from.1), Side::Left);
        t.update_selection(p(to.0, to.1), Side::Right);
    }

    #[test]
    fn simple_selection_within_a_line() {
        let mut t = term(20, 3);
        t.advance(b"hello world");
        select(&mut t, (0, 6), (0, 10));
        assert_eq!(t.selection_text().as_deref(), Some("world"));
        assert!(t.has_selection());
    }

    #[test]
    fn block_selection_takes_a_rectangle() {
        let mut t = term(20, 3);
        t.advance(b"abcdef\r\nghijkl\r\nmnopqr");
        t.start_selection(SelectionKind::Block, p(0, 1), Side::Left);
        t.update_selection(p(2, 3), Side::Right);
        assert_eq!(t.selection_text().as_deref(), Some("bcd\nhij\nnop"));
    }

    #[test]
    fn click_without_drag_selects_nothing() {
        let mut t = term(20, 3);
        t.advance(b"hello");
        t.start_selection(SelectionKind::Simple, p(0, 1), Side::Left);
        t.update_selection(p(0, 1), Side::Left);
        assert!(!t.has_selection());
        assert_eq!(t.selection_text(), None);
    }

    #[test]
    fn half_cells_decide_inclusion() {
        let mut t = term(20, 3);
        t.advance(b"abcdef");
        // From the right half of 'a' to the left half of 'e': "bcd".
        t.start_selection(SelectionKind::Simple, p(0, 0), Side::Right);
        t.update_selection(p(0, 4), Side::Left);
        assert_eq!(t.selection_text().as_deref(), Some("bcd"));
    }

    #[test]
    fn trailing_spaces_are_trimmed_per_line() {
        let mut t = term(20, 4);
        // Explicit trailing spaces written by the program, then a short line.
        t.advance(b"one   \x1b[41m  \x1b[0m\r\ntwo\r\n\r\nfour");
        select(&mut t, (0, 0), (3, 19));
        assert_eq!(t.selection_text().as_deref(), Some("one\ntwo\n\nfour"));
        // A selection ending inside a run of blanks.
        let mut t = term(20, 2);
        t.advance(b"ab   cd\r\nef");
        select(&mut t, (0, 0), (0, 4));
        assert_eq!(t.selection_text().as_deref(), Some("ab"));
        select(&mut t, (0, 3), (1, 1));
        assert_eq!(t.selection_text().as_deref(), Some("  cd\nef"));
    }

    #[test]
    fn soft_wrapped_lines_are_joined() {
        let mut t = term(5, 4);
        t.advance(b"abcdefgh\r\nxy");
        select(&mut t, (0, 0), (2, 4));
        assert_eq!(t.selection_text().as_deref(), Some("abcdefgh\nxy"));
    }

    #[test]
    fn wide_characters_are_copied_once() {
        let mut t = term(10, 2);
        t.advance("a日本b".as_bytes());
        select(&mut t, (0, 0), (0, 9));
        assert_eq!(t.selection_text().as_deref(), Some("a日本b"));
        // Starting on the spacer of 日 still includes the glyph.
        t.start_selection(SelectionKind::Simple, p(0, 2), Side::Left);
        t.update_selection(p(0, 4), Side::Right);
        assert_eq!(t.selection_text().as_deref(), Some("日本"));
    }

    #[test]
    fn wide_character_wrapped_to_next_line() {
        // 5 columns: "abcd" leaves one column, too narrow for 日, which wraps.
        let mut t = term(5, 3);
        t.advance("abcd日".as_bytes());
        select(&mut t, (0, 0), (1, 4));
        assert_eq!(t.selection_text().as_deref(), Some("abcd日"));
    }

    #[test]
    fn word_and_line_selection() {
        let mut t = term(30, 3);
        t.advance(b"cargo test --workspace now");
        t.start_selection(SelectionKind::Word, p(0, 8), Side::Left);
        assert_eq!(t.selection_text().as_deref(), Some("test"));
        t.start_selection(SelectionKind::Line, p(0, 3), Side::Left);
        assert_eq!(
            t.selection_text().as_deref(),
            Some("cargo test --workspace now\n")
        );
    }

    #[test]
    fn line_selection_follows_soft_wraps() {
        let mut t = term(5, 4);
        t.advance(b"abcdefgh\r\nxy");
        t.start_selection(SelectionKind::Line, p(1, 1), Side::Left);
        assert_eq!(t.selection_text().as_deref(), Some("abcdefgh\n"));
    }

    #[test]
    fn selection_survives_scrolling_into_history() {
        let mut t = term(10, 3);
        t.advance(b"keep\r\n");
        select(&mut t, (0, 0), (0, 3));
        for i in 0..10 {
            t.advance(format!("line{i}\r\n").as_bytes());
        }
        assert_eq!(t.selection_text().as_deref(), Some("keep"));

        // Selected while scrolled back, then viewed from the bottom.
        t.scroll(5);
        let s = t.snapshot();
        assert_eq!(s.row_text(0), "line3");
        select(&mut t, (0, 0), (1, 4));
        t.scroll_to_bottom();
        assert_eq!(t.selection_text().as_deref(), Some("line3\nline4"));
        t.advance(b"more\r\n");
        assert_eq!(t.selection_text().as_deref(), Some("line3\nline4"));
    }

    #[test]
    fn select_all_covers_scrollback_and_screen() {
        let mut t = term(10, 3);
        for i in 0..6 {
            t.advance(format!("l{i}\r\n").as_bytes());
        }
        t.advance(b"end");
        t.select_all();
        assert_eq!(
            t.selection_text().as_deref(),
            Some("l0\nl1\nl2\nl3\nl4\nl5\nend")
        );
    }

    #[test]
    fn overwriting_selected_cells_clears_selection() {
        let mut t = term(20, 3);
        t.advance(b"hello world");
        select(&mut t, (0, 6), (0, 10));
        // Output elsewhere on the line keeps it.
        t.advance(b"\r\x1b[1CX");
        assert_eq!(t.selection_text().as_deref(), Some("world"));
        // Rewriting the selected cells clears it.
        t.advance(b"\x1b[1;8HQ");
        assert!(!t.has_selection());
    }

    #[test]
    fn erasing_selected_line_clears_selection() {
        let mut t = term(20, 3);
        t.advance(b"hello world");
        select(&mut t, (0, 0), (0, 4));
        t.advance(b"\x1b[2K");
        assert!(!t.has_selection());
    }

    #[test]
    fn snapshot_marks_selected_cells() {
        let mut t = term(10, 2);
        t.advance("ab日c".as_bytes());
        select(&mut t, (0, 1), (0, 2));
        let s = t.snapshot();
        let selected: Vec<bool> = s
            .row(0)
            .iter()
            .map(|c| c.flags.contains(CellFlags::SELECTED))
            .collect();
        // 'b', 日 and its spacer.
        assert_eq!(
            selected,
            [
                false, true, true, true, false, false, false, false, false, false
            ]
        );
        t.clear_selection();
        assert!(
            !t.snapshot()
                .cells
                .iter()
                .any(|c| c.flags.contains(CellFlags::SELECTED))
        );
    }

    #[test]
    fn snapshot_selection_follows_display_offset() {
        let mut t = term(10, 2);
        t.advance(b"a\r\nb\r\nc");
        select(&mut t, (0, 0), (0, 0));
        // Viewport row 0 is "b" at the bottom; scrolled up by one it is row 1.
        t.scroll(1);
        let s = t.snapshot();
        assert_eq!(s.row_text(1), "b");
        assert!(s.row(1)[0].flags.contains(CellFlags::SELECTED));
        assert!(!s.row(0)[0].flags.contains(CellFlags::SELECTED));
    }
}
