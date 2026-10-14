//! GPUI view for a single PTY-backed terminal pane.
//!
//! Program output is parsed on `hangar-terminal`'s pump thread; this view only snapshots the
//! grid when GPUI paints. The pump wakes the view through a [`tokio::sync::Notify`], which
//! holds at most one pending permit, so bursts of output coalesce into one redraw.

mod element;
mod stats;

use std::ops::Range;
use std::sync::Arc;

use gpui_kit::*;
use hangar_terminal::{
    GridSize, Key, SpawnOptions, TerminalConfig, TerminalError, TerminalSession, encode_key,
    encode_paste,
};

use element::TerminalElement;
use stats::FrameStats;

/// Key context of the terminal; bindings in this context take precedence over app-wide ones.
pub const KEY_CONTEXT: &str = "Terminal";

/// Registers key bindings the terminal needs. Call once after `gpui_kit::init`.
///
/// gpui-kit's Root binds Tab / Shift-Tab to focus navigation; inside a terminal those keys
/// belong to the program (shell completion, Claude Code mode switching).
pub fn init(cx: &mut App) {
    cx.bind_keys([
        KeyBinding::new("tab", NoAction {}, Some(KEY_CONTEXT)),
        KeyBinding::new("shift-tab", NoAction {}, Some(KEY_CONTEXT)),
    ]);
}

/// Appearance and behaviour options.
#[derive(Clone, Debug)]
pub struct TerminalViewOptions {
    pub font_family: SharedString,
    pub font_size: Pixels,
    /// Line height as a multiple of the font size.
    pub line_height: f32,
    /// Treat Option as Meta (send ESC-prefixed keys) instead of composing characters.
    pub option_as_meta: bool,
    /// Log frame/byte/latency statistics to stderr once per second.
    pub stats: bool,
}

impl Default for TerminalViewOptions {
    fn default() -> Self {
        Self {
            font_family: "Menlo".into(),
            font_size: px(13.),
            line_height: 1.25,
            option_as_meta: false,
            stats: std::env::var_os("HANGAR_TERM_STATS").is_some_and(|v| v == "1"),
        }
    }
}

const PADDING: Pixels = px(4.);
/// Minimum spacing between redraw requests caused by program output (~one 120 Hz frame).
const REDRAW_INTERVAL: std::time::Duration = std::time::Duration::from_millis(8);

pub struct TerminalView {
    session: Arc<TerminalSession>,
    focus_handle: FocusHandle,
    options: TerminalViewOptions,
    font: Font,
    /// Cell size, resolved from font metrics on first paint.
    cell: Option<Size<Pixels>>,
    grid: GridSize,
    /// Where the cursor was last painted, for IME candidate windows.
    cursor_bounds: Option<Bounds<Pixels>>,
    /// In-progress IME composition (not yet sent to the program).
    marked_text: Option<String>,
    title: Option<String>,
    exit_message: Option<String>,
    scroll_remainder: Pixels,
    stats: Option<FrameStats>,
    _tasks: Vec<Task<()>>,
}

/// A started program, ready to be attached to a [`TerminalView`]. Spawning is separate from
/// view creation so failures (e.g. command not found) surface before a window opens.
pub struct StartedTerminal {
    session: Arc<TerminalSession>,
    wake: Arc<tokio::sync::Notify>,
}

impl StartedTerminal {
    pub fn spawn(spawn: &SpawnOptions) -> Result<Self, TerminalError> {
        let wake = Arc::new(tokio::sync::Notify::new());
        let notifier = Arc::clone(&wake);
        let session = Arc::new(TerminalSession::spawn(
            spawn,
            TerminalConfig::default(),
            move || notifier.notify_one(),
        )?);
        Ok(Self { session, wake })
    }
}

impl TerminalView {
    pub fn new(
        started: StartedTerminal,
        options: TerminalViewOptions,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let StartedTerminal { session, wake } = started;
        // The program starts at the spawn size; the first layout resizes PTY and grid.
        let grid = session.terminal().size();

        let mut tasks = vec![cx.spawn(async move |this, cx| {
            loop {
                wake.notified().await;
                let alive = this.update(cx, |view, cx| {
                    if let Some(stats) = view.stats.as_mut() {
                        stats.woke();
                    }
                    view.on_output();
                    cx.notify();
                });
                if alive.is_err() {
                    break;
                }
                // Output that keeps arriving within one frame is folded into the next redraw;
                // an isolated wake (e.g. a keystroke echo) is still handled immediately.
                cx.background_executor().timer(REDRAW_INTERVAL).await;
            }
        })];

        let stats = options
            .stats
            .then(|| FrameStats::new(Arc::clone(session.stats())));
        if stats.is_some() {
            tasks.push(cx.spawn(async move |this, cx| {
                loop {
                    cx.background_executor()
                        .timer(std::time::Duration::from_secs(1))
                        .await;
                    let alive = this.update_in(cx, |view, window, _| {
                        if let Some(stats) = view.stats.as_mut() {
                            stats.report(window.is_visible());
                        }
                    });
                    if alive.is_err() {
                        break;
                    }
                }
            }));
        }

        let focus_handle = cx.focus_handle();
        window.focus(&focus_handle, cx);

        let font = font(options.font_family.clone());
        Self {
            session,
            focus_handle,
            font,
            options,
            cell: None,
            grid,
            cursor_bounds: None,
            marked_text: None,
            title: None,
            exit_message: None,
            scroll_remainder: px(0.),
            stats,
            _tasks: tasks,
        }
    }

    pub fn session(&self) -> &Arc<TerminalSession> {
        &self.session
    }

    fn on_output(&mut self) {
        if self.exit_message.is_none()
            && let Some(status) = self.session.exit_status()
        {
            let message = match status {
                Ok(exit) if exit.success() => "[process exited]".to_owned(),
                Ok(exit) => match exit.signal {
                    Some(signal) => format!("[process killed by {signal}]"),
                    None => format!("[process exited with code {}]", exit.code),
                },
                Err(err) => format!("[process status unknown: {err}]"),
            };
            tracing::info!("{message}");
            self.exit_message = Some(message);
        }
    }

    /// Sends input to the program, snapping the viewport back to the live screen.
    fn send(&mut self, bytes: Vec<u8>) {
        if bytes.is_empty() || self.exit_message.is_some() {
            return;
        }
        self.session.terminal().scroll_to_bottom();
        if let Some(stats) = self.stats.as_mut() {
            stats.key_pressed();
        }
        if let Err(err) = self.session.write(bytes) {
            tracing::warn!(%err, "failed to write to terminal");
        }
    }

    fn on_key_down(&mut self, event: &KeyDownEvent, _: &mut Window, cx: &mut Context<Self>) {
        let keystroke = &event.keystroke;
        let m = keystroke.modifiers;

        if m.platform {
            if keystroke.key == "v" {
                self.paste(cx);
                cx.stop_propagation();
            }
            return;
        }

        let mods = hangar_terminal::Modifiers {
            shift: m.shift,
            alt: m.alt,
            ctrl: m.control,
        };
        let key = match named_key(&keystroke.key) {
            Some(key) => key,
            None => {
                // Plain characters arrive through the input handler (IME, dead keys, Option
                // compositions). Only Ctrl and Option-as-Meta combinations are encoded here.
                let meta = m.alt && self.options.option_as_meta;
                if !m.control && !meta {
                    return;
                }
                let Some(mut c) = single_char(&keystroke.key) else {
                    return;
                };
                if m.shift {
                    c = c.to_ascii_uppercase();
                }
                Key::Char(c)
            }
        };
        // Option + plain key without Option-as-Meta composes a character; let it through.
        if matches!(key, Key::Char(_)) && m.alt && !self.options.option_as_meta && !m.control {
            return;
        }
        let mode = self.session.terminal().key_mode();
        if let Some(bytes) = encode_key(key, mods, mode) {
            self.send(bytes);
            cx.stop_propagation();
        }
    }

    fn paste(&mut self, cx: &mut Context<Self>) {
        let Some(text) = cx.read_from_clipboard().and_then(|item| item.text()) else {
            return;
        };
        let bracketed = self.session.terminal().modes().bracketed_paste;
        self.send(encode_paste(&text, bracketed));
    }

    fn on_scroll(&mut self, event: &ScrollWheelEvent, _: &mut Window, cx: &mut Context<Self>) {
        let Some(cell) = self.cell else { return };
        let delta = event.delta.pixel_delta(cell.height).y + self.scroll_remainder;
        let lines = (f32::from(delta) / f32::from(cell.height)).trunc();
        self.scroll_remainder = delta - cell.height * lines;
        let lines = lines as i32;
        if lines == 0 {
            return;
        }
        let modes = self.session.terminal().modes();
        if modes.alt_screen && modes.alternate_scroll && !modes.mouse_reporting {
            // Full-screen programs without mouse support get arrow keys, like xterm.
            let key = if lines > 0 { Key::Up } else { Key::Down };
            let mode = self.session.terminal().key_mode();
            let bytes: Vec<u8> = (0..lines.unsigned_abs())
                .filter_map(|_| encode_key(key, hangar_terminal::Modifiers::NONE, mode))
                .flatten()
                .collect();
            if let Err(err) = self.session.write(bytes) {
                tracing::warn!(%err, "failed to write to terminal");
            }
        } else {
            self.session.terminal().scroll(lines);
        }
        cx.notify();
    }
}

fn named_key(key: &str) -> Option<Key> {
    let key = match key {
        "enter" => Key::Enter,
        "tab" => Key::Tab,
        "backspace" => Key::Backspace,
        "escape" => Key::Escape,
        "up" => Key::Up,
        "down" => Key::Down,
        "left" => Key::Left,
        "right" => Key::Right,
        "home" => Key::Home,
        "end" => Key::End,
        "pageup" => Key::PageUp,
        "pagedown" => Key::PageDown,
        "insert" => Key::Insert,
        "delete" => Key::Delete,
        "space" => Key::Char(' '),
        f if f.len() >= 2 && f.starts_with('f') => match f[1..].parse::<u8>() {
            Ok(n @ 1..=12) => Key::F(n),
            _ => return None,
        },
        _ => return None,
    };
    Some(key)
}

fn single_char(key: &str) -> Option<char> {
    let mut chars = key.chars();
    let c = chars.next()?;
    chars.next().is_none().then_some(c)
}

impl Focusable for TerminalView {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for TerminalView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let background = self.session.terminal().palette().background;
        div()
            .id("terminal")
            .key_context(KEY_CONTEXT)
            .track_focus(&self.focus_handle)
            .size_full()
            .p(PADDING)
            .bg(rgb(background.to_hex()))
            .on_key_down(cx.listener(Self::on_key_down))
            .on_scroll_wheel(cx.listener(Self::on_scroll))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|view, _: &MouseDownEvent, window, cx| {
                    window.focus(&view.focus_handle, cx);
                }),
            )
            .child(TerminalElement::new(cx.entity()))
    }
}

impl EntityInputHandler for TerminalView {
    fn text_for_range(
        &mut self,
        _range: Range<usize>,
        _adjusted_range: &mut Option<Range<usize>>,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<String> {
        None
    }

    fn selected_text_range(
        &mut self,
        _ignore_disabled_input: bool,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<UTF16Selection> {
        let end = self
            .marked_text
            .as_ref()
            .map_or(0, |text| text.encode_utf16().count());
        Some(UTF16Selection {
            range: end..end,
            reversed: false,
        })
    }

    fn marked_text_range(
        &self,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<Range<usize>> {
        self.marked_text
            .as_ref()
            .map(|text| 0..text.encode_utf16().count())
    }

    fn unmark_text(&mut self, _window: &mut Window, cx: &mut Context<Self>) {
        if self.marked_text.take().is_some() {
            cx.notify();
        }
    }

    fn replace_text_in_range(
        &mut self,
        _range: Option<Range<usize>>,
        text: &str,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.marked_text = None;
        self.send(text.as_bytes().to_vec());
        cx.notify();
    }

    fn replace_and_mark_text_in_range(
        &mut self,
        _range: Option<Range<usize>>,
        new_text: &str,
        _new_selected_range: Option<Range<usize>>,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.marked_text = (!new_text.is_empty()).then(|| new_text.to_owned());
        cx.notify();
    }

    fn bounds_for_range(
        &mut self,
        _range_utf16: Range<usize>,
        _element_bounds: Bounds<Pixels>,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<Bounds<Pixels>> {
        self.cursor_bounds
    }

    fn character_index_for_point(
        &mut self,
        _point: Point<Pixels>,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<usize> {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::{named_key, single_char};
    use hangar_terminal::Key;

    #[test]
    fn named_keys_map_to_terminal_keys() {
        assert_eq!(named_key("enter"), Some(Key::Enter));
        assert_eq!(named_key("f5"), Some(Key::F(5)));
        assert_eq!(named_key("f13"), None);
        assert_eq!(named_key("f"), None);
        assert_eq!(named_key("a"), None);
        assert_eq!(named_key("space"), Some(Key::Char(' ')));
    }

    #[test]
    fn single_char_rejects_names() {
        assert_eq!(single_char("a"), Some('a'));
        assert_eq!(single_char("é"), Some('é'));
        assert_eq!(single_char("up"), None);
        assert_eq!(single_char(""), None);
    }
}
