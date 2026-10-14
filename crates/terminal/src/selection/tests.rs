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
fn block_selection_dragged_up_the_last_column_is_empty() {
    let mut t = term(80, 24);
    t.advance(b"x");
    t.start_selection(SelectionKind::Block, p(5, 79), Side::Left);
    t.update_selection(p(1, 79), Side::Right);
    assert_eq!(t.selection_text(), None);
    assert!(!t.has_selection());
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
