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

/// Longest window title a program may set (OSC 0/2), in characters.
pub const MAX_TITLE_CHARS: usize = 256;
/// Largest clipboard write a program may ask for (OSC 52), in bytes.
pub const MAX_CLIPBOARD_BYTES: usize = 1024 * 1024;

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
            Event::Title(title) => {
                state.title = Some(title.chars().take(MAX_TITLE_CHARS).collect());
            }
            Event::ResetTitle => state.title = None,
            Event::ClipboardStore(_, text) if text.len() <= MAX_CLIPBOARD_BYTES => {
                state.clipboard_store = Some(text);
            }
            Event::ClipboardStore(_, text) => {
                tracing::warn!(
                    bytes = text.len(),
                    "ignored an oversized clipboard write (OSC 52)"
                );
            }
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
mod tests;
