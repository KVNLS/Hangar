use std::sync::{Arc, Mutex};
use std::time::Instant;

use alacritty_terminal::Term;
use alacritty_terminal::event::{Event, EventListener, WindowSize};
use alacritty_terminal::grid::{Dimensions, Scroll};
use alacritty_terminal::index::{Column, Line, Point};
use alacritty_terminal::term::cell::Flags;
use alacritty_terminal::term::{Config, TermMode, point_to_viewport};
use alacritty_terminal::vte::ansi::{self, CursorShape as AnsiCursorShape, Processor};

use crate::error::{Result, TerminalError};
use crate::keys::KeyMode;
use crate::lock;
use crate::mouse::{MouseEncoding, MouseMode, MouseTracking};
use crate::palette::Palette;
use crate::snapshot::{CellFlags, CursorShape, CursorState, Snapshot, SnapshotCell, TermModes};

/// Size of the character grid.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct GridSize {
    pub cols: u16,
    pub rows: u16,
}

impl GridSize {
    pub fn new(cols: u16, rows: u16) -> Result<Self> {
        if cols == 0 || rows == 0 {
            return Err(TerminalError::InvalidSize { cols, rows });
        }
        Ok(Self { cols, rows })
    }
}

impl Default for GridSize {
    fn default() -> Self {
        Self { cols: 80, rows: 24 }
    }
}

impl Dimensions for GridSize {
    fn total_lines(&self) -> usize {
        usize::from(self.rows)
    }

    fn screen_lines(&self) -> usize {
        usize::from(self.rows)
    }

    fn columns(&self) -> usize {
        usize::from(self.cols)
    }
}

#[derive(Clone, Debug)]
pub struct TerminalConfig {
    /// Lines of scrollback history kept above the screen.
    pub scrollback: usize,
    pub palette: Palette,
}

impl Default for TerminalConfig {
    fn default() -> Self {
        Self {
            scrollback: 10_000,
            palette: Palette::default(),
        }
    }
}

type ColorFormatter = Arc<dyn Fn(ansi::Rgb) -> String + Sync + Send + 'static>;
type SizeFormatter = Arc<dyn Fn(WindowSize) -> String + Sync + Send + 'static>;

#[derive(Default)]
struct ListenerState {
    replies: Vec<u8>,
    color_requests: Vec<(usize, ColorFormatter)>,
    size_requests: Vec<SizeFormatter>,
    title: Option<String>,
    clipboard_store: Option<String>,
    bell: bool,
}

#[derive(Clone, Default)]
pub(crate) struct Listener(Arc<Mutex<ListenerState>>);

impl EventListener for Listener {
    fn send_event(&self, event: Event) {
        let mut state = lock(&self.0);
        match event {
            Event::PtyWrite(text) => state.replies.extend_from_slice(text.as_bytes()),
            Event::ColorRequest(index, format) => state.color_requests.push((index, format)),
            Event::TextAreaSizeRequest(format) => state.size_requests.push(format),
            Event::Title(title) => state.title = Some(title),
            Event::ResetTitle => state.title = None,
            Event::ClipboardStore(_, text) => state.clipboard_store = Some(text),
            Event::Bell => state.bell = true,
            // ClipboardLoad is never emitted: OSC 52 paste is disabled by the default config.
            // The rest concern renderer state that is recomputed on every snapshot.
            Event::ClipboardLoad(..)
            | Event::MouseCursorDirty
            | Event::CursorBlinkingChange
            | Event::Wakeup
            | Event::Exit
            | Event::ChildExit(_) => {}
        }
    }
}

/// Terminal emulator state: an alacritty `Term` plus its escape-sequence parser.
///
/// Pure data: feeding bytes never performs I/O. Bytes the emulator wants to send back to the
/// program (device status reports, colour queries) are collected and returned by
/// [`Terminal::take_replies`].
pub struct Terminal {
    pub(crate) term: Term<Listener>,
    parser: Processor,
    listener: Listener,
    palette: Palette,
    size: GridSize,
    cell_px: (u16, u16),
}

impl Terminal {
    pub fn new(size: GridSize, config: TerminalConfig) -> Self {
        let listener = Listener::default();
        let term_config = Config {
            scrolling_history: config.scrollback,
            ..Config::default()
        };
        Self {
            term: Term::new(term_config, &size, listener.clone()),
            parser: Processor::new(),
            listener,
            palette: config.palette,
            size,
            cell_px: (0, 0),
        }
    }

    /// Feeds program output into the emulator.
    pub fn advance(&mut self, bytes: &[u8]) {
        let selection = self.selection_fingerprint();
        self.parser.advance(&mut self.term, bytes);
        if let Some(selection) = selection {
            self.drop_overwritten_selection(selection);
        }
        self.answer_queries();
    }

    /// Deadline of a pending synchronized update (DEC mode 2026), if any. Output is buffered
    /// until the program ends the update or this deadline passes.
    pub fn sync_deadline(&self) -> Option<Instant> {
        self.parser.sync_timeout().sync_timeout()
    }

    /// Applies a synchronized update whose deadline has passed.
    pub fn flush_expired_sync(&mut self) {
        if self.sync_deadline().is_some_and(|d| d <= Instant::now()) {
            let selection = self.selection_fingerprint();
            self.parser.stop_sync(&mut self.term);
            if let Some(selection) = selection {
                self.drop_overwritten_selection(selection);
            }
            self.answer_queries();
        }
    }

    pub fn resize(&mut self, size: GridSize, cell_px: (u16, u16)) {
        self.cell_px = cell_px;
        if size != self.size {
            self.size = size;
            self.term.resize(size);
        }
    }

    pub fn size(&self) -> GridSize {
        self.size
    }

    /// Scrolls the viewport; positive values move up into the scrollback.
    pub fn scroll(&mut self, lines: i32) {
        if lines != 0 {
            self.term.scroll_display(Scroll::Delta(lines));
        }
    }

    pub fn scroll_to_bottom(&mut self) {
        if self.term.grid().display_offset() != 0 {
            self.term.scroll_display(Scroll::Bottom);
        }
    }

    pub fn display_offset(&self) -> usize {
        self.term.grid().display_offset()
    }

    pub fn history_size(&self) -> usize {
        self.term.grid().history_size()
    }

    pub fn modes(&self) -> TermModes {
        modes_from(*self.term.mode())
    }

    /// Mouse tracking requested by the program.
    pub fn mouse_mode(&self) -> MouseMode {
        mouse_mode_from(*self.term.mode())
    }

    pub fn key_mode(&self) -> KeyMode {
        KeyMode {
            app_cursor: self.term.mode().contains(TermMode::APP_CURSOR),
        }
    }

    /// Bytes the emulator must write back to the program (replies to queries).
    pub fn take_replies(&mut self) -> Vec<u8> {
        std::mem::take(&mut lock(&self.listener.0).replies)
    }

    /// Current window title set by the program (OSC 0/2).
    pub fn title(&self) -> Option<String> {
        lock(&self.listener.0).title.clone()
    }

    /// Text the program asked to put on the clipboard (OSC 52), consumed once.
    pub fn take_clipboard_store(&mut self) -> Option<String> {
        lock(&self.listener.0).clipboard_store.take()
    }

    /// Whether the bell rang since the last call.
    pub fn take_bell(&mut self) -> bool {
        std::mem::take(&mut lock(&self.listener.0).bell)
    }

    pub fn palette(&self) -> &Palette {
        &self.palette
    }

    /// Replaces the default colours (theme change). Program overrides (OSC 4/10/11/12) still win.
    pub fn set_palette(&mut self, palette: Palette) {
        self.palette = palette;
    }

    /// The last `max` lines of text, scrollback included (the alternate screen has none), oldest
    /// first. Soft-wrapped rows are joined; trailing spaces and trailing empty lines are dropped.
    pub fn recent_lines(&self, max: usize) -> Vec<String> {
        let grid = self.term.grid();
        let cols = grid.columns();
        let top = -(grid.history_size() as i32);
        let bottom = grid.screen_lines() as i32;
        let mut lines: Vec<String> = Vec::new();
        let mut current = String::new();
        for row in top..bottom {
            let grid_row = &grid[Line(row)];
            for col in 0..cols {
                let cell = &grid_row[Column(col)];
                if cell.flags.contains(Flags::WIDE_CHAR_SPACER) {
                    continue;
                }
                current.push(cell.c);
                if let Some(zerowidth) = cell.zerowidth() {
                    current.extend(zerowidth.iter());
                }
            }
            let wrapped = cols > 0 && grid_row[Column(cols - 1)].flags.contains(Flags::WRAPLINE);
            if !wrapped {
                current.truncate(current.trim_end().len());
                lines.push(std::mem::take(&mut current));
            }
        }
        if !current.is_empty() {
            current.truncate(current.trim_end().len());
            lines.push(current);
        }
        while lines.last().is_some_and(String::is_empty) {
            lines.pop();
        }
        let skip = lines.len().saturating_sub(max);
        lines.split_off(skip)
    }

    /// Plain-data copy of the visible screen, with colours resolved to RGB.
    pub fn snapshot(&self) -> Snapshot {
        let grid = self.term.grid();
        let cols = grid.columns();
        let rows = grid.screen_lines();
        let display_offset = grid.display_offset();
        let overrides = self.term.colors();
        let palette = &self.palette;
        let background =
            palette.resolve(ansi::Color::Named(ansi::NamedColor::Background), overrides);
        let foreground =
            palette.resolve(ansi::Color::Named(ansi::NamedColor::Foreground), overrides);
        let cursor_color = palette.resolve(ansi::Color::Named(ansi::NamedColor::Cursor), overrides);

        let selection = self.selection_range();
        let mut cells = Vec::with_capacity(cols * rows);
        for row in 0..rows {
            let line = Line(row as i32 - display_offset as i32);
            let grid_row = &grid[line];
            for col in 0..cols {
                let cell = &grid_row[Column(col)];
                let selected = selection.is_some_and(|range| {
                    let at = |c: usize| range.contains(Point::new(line, Column(c)));
                    at(col)
                        || (cell.flags.contains(Flags::WIDE_CHAR) && at(col + 1))
                        || (cell.flags.contains(Flags::WIDE_CHAR_SPACER) && col > 0 && at(col - 1))
                });
                let mut fg = if cell.flags.contains(Flags::DIM) {
                    palette.resolve_dim(cell.fg, overrides)
                } else {
                    palette.resolve(cell.fg, overrides)
                };
                let mut bg = palette.resolve(cell.bg, overrides);
                if cell.flags.contains(Flags::INVERSE) {
                    std::mem::swap(&mut fg, &mut bg);
                }
                if cell.flags.contains(Flags::HIDDEN) {
                    fg = bg;
                }
                cells.push(SnapshotCell {
                    ch: cell.c,
                    zerowidth: cell.zerowidth().map(<[char]>::to_vec).unwrap_or_default(),
                    fg,
                    bg,
                    flags: cell_flags(cell.flags, selected),
                });
            }
        }

        let content = self.term.renderable_content();
        let cursor = cursor_shape(content.cursor.shape).and_then(|shape| {
            let point = point_to_viewport(display_offset, content.cursor.point)?;
            (point.line < rows).then_some(CursorState {
                row: point.line,
                col: point.column.0,
                shape,
            })
        });

        Snapshot {
            cols,
            rows,
            cells,
            cursor,
            display_offset,
            history_size: grid.history_size(),
            foreground,
            background,
            cursor_color,
            modes: modes_from(content.mode),
        }
    }

    fn answer_queries(&mut self) {
        let mut state = lock(&self.listener.0);
        if state.color_requests.is_empty() && state.size_requests.is_empty() {
            return;
        }
        let overrides = self.term.colors();
        for (index, format) in std::mem::take(&mut state.color_requests) {
            match self.palette.resolve_index(index, overrides) {
                Some(rgb) => {
                    let reply = format(rgb.into());
                    state.replies.extend_from_slice(reply.as_bytes());
                }
                None => tracing::debug!(index, "ignoring colour query for unknown index"),
            }
        }
        let window = WindowSize {
            num_lines: self.size.rows,
            num_cols: self.size.cols,
            cell_width: self.cell_px.0,
            cell_height: self.cell_px.1,
        };
        for format in std::mem::take(&mut state.size_requests) {
            let reply = format(window);
            state.replies.extend_from_slice(reply.as_bytes());
        }
    }
}

fn cell_flags(flags: Flags, selected: bool) -> CellFlags {
    let mut out = CellFlags::empty();
    if selected {
        out.insert(CellFlags::SELECTED);
    }
    let map = [
        (Flags::BOLD, CellFlags::BOLD),
        (Flags::ITALIC, CellFlags::ITALIC),
        (Flags::ALL_UNDERLINES, CellFlags::UNDERLINE),
        (Flags::UNDERCURL, CellFlags::UNDERCURL),
        (Flags::STRIKEOUT, CellFlags::STRIKEOUT),
        (Flags::DIM, CellFlags::DIM),
        (Flags::INVERSE, CellFlags::INVERSE),
        (Flags::WIDE_CHAR, CellFlags::WIDE),
        (
            Flags::WIDE_CHAR_SPACER.union(Flags::LEADING_WIDE_CHAR_SPACER),
            CellFlags::WIDE_SPACER,
        ),
    ];
    for (from, to) in map {
        if flags.intersects(from) {
            out.insert(to);
        }
    }
    out
}

fn cursor_shape(shape: AnsiCursorShape) -> Option<CursorShape> {
    match shape {
        AnsiCursorShape::Block => Some(CursorShape::Block),
        AnsiCursorShape::Underline => Some(CursorShape::Underline),
        AnsiCursorShape::Beam => Some(CursorShape::Beam),
        AnsiCursorShape::HollowBlock => Some(CursorShape::HollowBlock),
        AnsiCursorShape::Hidden => None,
    }
}

fn modes_from(mode: TermMode) -> TermModes {
    TermModes {
        app_cursor: mode.contains(TermMode::APP_CURSOR),
        bracketed_paste: mode.contains(TermMode::BRACKETED_PASTE),
        alt_screen: mode.contains(TermMode::ALT_SCREEN),
        alternate_scroll: mode.contains(TermMode::ALTERNATE_SCROLL),
        mouse_reporting: mode.intersects(TermMode::MOUSE_MODE),
        focus_reporting: mode.contains(TermMode::FOCUS_IN_OUT),
        mouse: mouse_mode_from(mode),
    }
}

fn mouse_mode_from(mode: TermMode) -> MouseMode {
    let tracking = if mode.contains(TermMode::MOUSE_MOTION) {
        MouseTracking::Motion
    } else if mode.contains(TermMode::MOUSE_DRAG) {
        MouseTracking::Drag
    } else if mode.contains(TermMode::MOUSE_REPORT_CLICK) {
        MouseTracking::Click
    } else {
        MouseTracking::Off
    };
    let encoding = if mode.contains(TermMode::SGR_MOUSE) {
        MouseEncoding::Sgr
    } else if mode.contains(TermMode::UTF8_MOUSE) {
        MouseEncoding::Utf8
    } else {
        MouseEncoding::X10
    };
    MouseMode { tracking, encoding }
}

#[cfg(test)]
mod tests {
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
}
