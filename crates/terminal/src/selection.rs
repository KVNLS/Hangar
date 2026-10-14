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
mod tests;
