use crate::palette::Rgb;

/// Text attributes of a cell. Colours are already resolved (inverse/hidden applied).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct CellFlags(u16);

impl CellFlags {
    pub const BOLD: Self = Self(1);
    pub const ITALIC: Self = Self(1 << 1);
    /// Any underline style (single, double, dotted, dashed, curly).
    pub const UNDERLINE: Self = Self(1 << 2);
    pub const UNDERCURL: Self = Self(1 << 3);
    pub const STRIKEOUT: Self = Self(1 << 4);
    pub const DIM: Self = Self(1 << 5);
    /// Informational only: `fg`/`bg` are already swapped.
    pub const INVERSE: Self = Self(1 << 6);
    /// The glyph occupies this cell and the next one.
    pub const WIDE: Self = Self(1 << 7);
    /// Placeholder cell after (or before a wrapped) wide glyph; draw nothing.
    pub const WIDE_SPACER: Self = Self(1 << 8);

    pub const fn empty() -> Self {
        Self(0)
    }

    pub const fn contains(self, other: Self) -> bool {
        self.0 & other.0 == other.0
    }

    pub fn insert(&mut self, other: Self) {
        self.0 |= other.0;
    }

    pub fn remove(&mut self, other: Self) {
        self.0 &= !other.0;
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SnapshotCell {
    pub ch: char,
    /// Combining characters attached to `ch`.
    pub zerowidth: Vec<char>,
    pub fg: Rgb,
    pub bg: Rgb,
    pub flags: CellFlags,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CursorShape {
    Block,
    Underline,
    Beam,
    HollowBlock,
}

/// Cursor position in viewport coordinates.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CursorState {
    pub row: usize,
    pub col: usize,
    pub shape: CursorShape,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct TermModes {
    pub app_cursor: bool,
    pub bracketed_paste: bool,
    pub alt_screen: bool,
    pub alternate_scroll: bool,
    pub mouse_reporting: bool,
    pub focus_reporting: bool,
}

/// The visible screen as plain data.
#[derive(Clone, Debug)]
pub struct Snapshot {
    pub cols: usize,
    pub rows: usize,
    /// `rows * cols` cells, row-major.
    pub cells: Vec<SnapshotCell>,
    /// `None` when hidden (DECTCEM) or scrolled out of view.
    pub cursor: Option<CursorState>,
    /// Lines scrolled back into history (0 = live screen).
    pub display_offset: usize,
    pub history_size: usize,
    pub foreground: Rgb,
    pub background: Rgb,
    pub cursor_color: Rgb,
    pub modes: TermModes,
}

impl Snapshot {
    pub fn row(&self, row: usize) -> &[SnapshotCell] {
        let start = row * self.cols;
        self.cells.get(start..start + self.cols).unwrap_or(&[])
    }

    /// Text of a row with trailing blanks trimmed; wide-glyph spacers are skipped.
    pub fn row_text(&self, row: usize) -> String {
        let mut text = String::with_capacity(self.cols);
        for cell in self.row(row) {
            if cell.flags.contains(CellFlags::WIDE_SPACER) {
                continue;
            }
            text.push(cell.ch);
            text.extend(cell.zerowidth.iter());
        }
        text.truncate(text.trim_end().len());
        text
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn flag_set_operations() {
        let mut f = CellFlags::empty();
        f.insert(CellFlags::BOLD);
        f.insert(CellFlags::ITALIC);
        assert!(f.contains(CellFlags::BOLD));
        f.remove(CellFlags::BOLD);
        assert!(!f.contains(CellFlags::BOLD));
        assert!(f.contains(CellFlags::ITALIC));
    }
}
