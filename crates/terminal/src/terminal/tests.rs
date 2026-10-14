use super::*;
use crate::palette::Rgb;

fn term(cols: u16, rows: u16) -> Terminal {
    Terminal::new(
        GridSize::new(cols, rows).unwrap(),
        TerminalConfig::default(),
    )
}

fn row_text(s: &Snapshot, row: usize) -> String {
    s.row_text(row)
}

#[test]
fn recent_lines_include_scrollback_and_join_wrapped_rows() {
    let mut t = term(10, 3);
    for i in 1..=6 {
        t.advance(format!("line {i}\r\n").as_bytes());
    }
    // 14 characters on a 10-column screen: one soft-wrapped line.
    t.advance(b"0123456789abcd");
    assert_eq!(
        t.recent_lines(100),
        [
            "line 1",
            "line 2",
            "line 3",
            "line 4",
            "line 5",
            "line 6",
            "0123456789abcd"
        ]
    );
    assert_eq!(t.recent_lines(2), ["line 6", "0123456789abcd"]);
    assert!(term(10, 3).recent_lines(5).is_empty());
}

#[test]
fn plain_text_lands_in_grid() {
    let mut t = term(20, 5);
    t.advance(b"hello\r\nworld");
    let s = t.snapshot();
    assert_eq!((s.cols, s.rows), (20, 5));
    assert_eq!(row_text(&s, 0), "hello");
    assert_eq!(row_text(&s, 1), "world");
    let cursor = s.cursor.unwrap();
    assert_eq!((cursor.row, cursor.col), (1, 5));
    assert_eq!(cursor.shape, CursorShape::Block);
}

#[test]
fn sgr_attributes_and_colours() {
    let mut t = term(40, 3);
    let p = Palette::default();
    // red, bold; 256-colour 196 bg; truecolor fg; italic+underline; inverse.
    t.advance(b"\x1b[1;31mA\x1b[0m\x1b[48;5;196mB\x1b[0m\x1b[38;2;10;20;30mC\x1b[0m");
    t.advance(b"\x1b[3;4mD\x1b[0m\x1b[7mE\x1b[0m\x1b[2mF\x1b[0m\x1b[9mG");
    let s = t.snapshot();
    let row = s.row(0);
    assert_eq!(row[0].ch, 'A');
    assert_eq!(row[0].fg, p.ansi[1]);
    assert!(row[0].flags.contains(CellFlags::BOLD));
    assert_eq!(row[1].bg, Rgb::new(255, 0, 0));
    assert_eq!(row[2].fg, Rgb::new(10, 20, 30));
    assert!(row[3].flags.contains(CellFlags::ITALIC));
    assert!(row[3].flags.contains(CellFlags::UNDERLINE));
    assert_eq!(row[4].fg, p.background);
    assert_eq!(row[4].bg, p.foreground);
    assert!(row[4].flags.contains(CellFlags::INVERSE));
    assert!(row[5].flags.contains(CellFlags::DIM));
    assert_ne!(row[5].fg, p.foreground);
    assert!(row[6].flags.contains(CellFlags::STRIKEOUT));
    assert_eq!(row[7].bg, p.background);
}

#[test]
fn wide_characters_are_flagged() {
    let mut t = term(10, 2);
    t.advance("日x".as_bytes());
    let s = t.snapshot();
    let row = s.row(0);
    assert_eq!(row[0].ch, '日');
    assert!(row[0].flags.contains(CellFlags::WIDE));
    assert!(row[1].flags.contains(CellFlags::WIDE_SPACER));
    assert_eq!(row[2].ch, 'x');
}

#[test]
fn scrollback_and_display_offset() {
    let mut t = term(10, 3);
    for i in 0..10 {
        t.advance(format!("line{i}\r\n").as_bytes());
    }
    let s = t.snapshot();
    assert_eq!(row_text(&s, 0), "line8");
    assert_eq!(row_text(&s, 1), "line9");
    assert_eq!(s.history_size, 8);

    t.scroll(2);
    let s = t.snapshot();
    assert_eq!(s.display_offset, 2);
    assert_eq!(row_text(&s, 0), "line6");
    // Cursor is on the bottom row of the live screen, which is now below the viewport.
    assert!(s.cursor.is_none());

    t.scroll(1000);
    assert_eq!(t.display_offset(), 8);
    t.scroll_to_bottom();
    assert_eq!(t.display_offset(), 0);
}

#[test]
fn scrollback_is_capped() {
    let mut t = Terminal::new(
        GridSize::new(10, 2).unwrap(),
        TerminalConfig {
            scrollback: 5,
            ..TerminalConfig::default()
        },
    );
    for i in 0..50 {
        t.advance(format!("{i}\r\n").as_bytes());
    }
    assert_eq!(t.history_size(), 5);
}

#[test]
fn resize_changes_grid() {
    let mut t = term(10, 3);
    t.advance(b"abc");
    t.resize(GridSize::new(30, 8).unwrap(), (8, 16));
    let s = t.snapshot();
    assert_eq!((s.cols, s.rows), (30, 8));
    assert_eq!(s.cells.len(), 240);
    assert_eq!(row_text(&s, 0), "abc");
}

#[test]
fn device_status_report_produces_reply() {
    let mut t = term(10, 3);
    t.advance(b"ab\x1b[6n");
    assert_eq!(t.take_replies(), b"\x1b[1;3R");
    assert!(t.take_replies().is_empty());
}

#[test]
fn background_colour_query_is_answered() {
    let mut t = term(10, 3);
    t.advance(b"\x1b]11;?\x07");
    let reply = String::from_utf8(t.take_replies()).unwrap();
    // Palette::default background is #16181c.
    assert!(reply.starts_with("\x1b]11;rgb:1616/1818/1c1c"), "{reply:?}");
}

#[test]
fn set_palette_changes_defaults_but_not_program_overrides() {
    let mut t = term(10, 3);
    t.set_palette(Palette {
        background: Rgb::from_hex(0x0f1013),
        ..Palette::default()
    });
    assert_eq!(t.snapshot().background, Rgb::from_hex(0x0f1013));
    t.advance(b"\x1b]11;?\x07");
    let reply = String::from_utf8(t.take_replies()).unwrap();
    assert!(reply.starts_with("\x1b]11;rgb:0f0f/1010/1313"), "{reply:?}");
    t.advance(b"\x1b]11;#ff0000\x07");
    t.set_palette(Palette::default());
    assert_eq!(t.snapshot().background, Rgb::from_hex(0xff0000));
}

#[test]
fn modes_and_cursor_visibility() {
    let mut t = term(10, 3);
    assert!(!t.key_mode().app_cursor);
    t.advance(b"\x1b[?1h\x1b[?2004h\x1b[?25l");
    assert!(t.key_mode().app_cursor);
    let s = t.snapshot();
    assert!(s.modes.bracketed_paste);
    assert!(s.cursor.is_none());
    t.advance(b"\x1b[?1049h");
    assert!(t.modes().alt_screen);
}

#[test]
fn mouse_and_focus_modes() {
    let mut t = term(10, 3);
    assert_eq!(t.mouse_mode(), MouseMode::default());
    t.advance(b"\x1b[?1000h");
    assert_eq!(t.mouse_mode().tracking, MouseTracking::Click);
    assert_eq!(t.mouse_mode().encoding, MouseEncoding::X10);
    t.advance(b"\x1b[?1002h\x1b[?1006h");
    assert_eq!(
        t.mouse_mode(),
        MouseMode {
            tracking: MouseTracking::Drag,
            encoding: MouseEncoding::Sgr
        }
    );
    t.advance(b"\x1b[?1003h\x1b[?1005h");
    assert_eq!(
        t.mouse_mode(),
        MouseMode {
            tracking: MouseTracking::Motion,
            encoding: MouseEncoding::Utf8
        }
    );
    assert!(t.modes().mouse_reporting);
    t.advance(b"\x1b[?1003l\x1b[?1005l");
    assert_eq!(t.mouse_mode(), MouseMode::default());
    assert!(!t.modes().focus_reporting);
    t.advance(b"\x1b[?1004h");
    assert!(t.modes().focus_reporting);
}

#[test]
fn title_and_cursor_shape() {
    let mut t = term(10, 3);
    t.advance(b"\x1b]0;my title\x07\x1b[6 q");
    assert_eq!(t.title().as_deref(), Some("my title"));
    assert_eq!(t.snapshot().cursor.unwrap().shape, CursorShape::Beam);
}

#[test]
fn synchronized_update_is_buffered_until_end() {
    let mut t = term(10, 3);
    t.advance(b"\x1b[?2026hhidden");
    assert!(t.sync_deadline().is_some());
    assert_eq!(row_text(&t.snapshot(), 0), "");
    t.advance(b"\x1b[?2026l");
    assert!(t.sync_deadline().is_none());
    assert_eq!(row_text(&t.snapshot(), 0), "hidden");
}

#[test]
fn titles_and_clipboard_writes_are_capped() {
    let mut t = term(10, 3);
    t.advance(format!("\x1b]2;{}\x07", "t".repeat(10_000)).as_bytes());
    assert_eq!(t.title().unwrap().chars().count(), MAX_TITLE_CHARS);

    let small = "aGk="; // "hi"
    t.advance(format!("\x1b]52;c;{small}\x07").as_bytes());
    assert_eq!(t.take_clipboard_store().as_deref(), Some("hi"));
    let huge = "A".repeat(MAX_CLIPBOARD_BYTES * 2);
    t.advance(format!("\x1b]52;c;{huge}\x07").as_bytes());
    assert_eq!(t.take_clipboard_store(), None);
}
