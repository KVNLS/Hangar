//! Custom element painting the terminal grid: background quads, one shaped line per style
//! run (glyphs pinned to the cell grid with `force_width`), the selection and the cursor.
//! It also owns the pointer hitbox and registers the window mouse listeners, so drags keep
//! working outside its bounds.

use gpui_kit::*;
use hangar_terminal::{CellFlags, CursorShape, GridSize, Rgb, Snapshot, SnapshotCell};

use super::TerminalView;

pub(super) struct TerminalElement {
    view: Entity<TerminalView>,
}

impl TerminalElement {
    pub(super) fn new(view: Entity<TerminalView>) -> Self {
        Self { view }
    }
}

impl IntoElement for TerminalElement {
    type Element = Self;

    fn into_element(self) -> Self::Element {
        self
    }
}

pub(super) struct Frame {
    focus_handle: FocusHandle,
    hitbox: Option<Hitbox>,
    background: Hsla,
    quads: Vec<PaintQuad>,
    lines: Vec<(Point<Pixels>, ShapedLine)>,
    overlays: Vec<PaintQuad>,
    line_height: Pixels,
    bytes_in_frame: u64,
}

impl Element for TerminalElement {
    type RequestLayoutState = ();
    type PrepaintState = Frame;

    fn id(&self) -> Option<ElementId> {
        None
    }

    fn source_location(&self) -> Option<&'static core::panic::Location<'static>> {
        None
    }

    fn request_layout(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (LayoutId, Self::RequestLayoutState) {
        let mut style = Style::default();
        style.size.width = relative(1.).into();
        style.size.height = relative(1.).into();
        (window.request_layout(style, [], cx), ())
    }

    fn prepaint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        _request_layout: &mut Self::RequestLayoutState,
        window: &mut Window,
        cx: &mut App,
    ) -> Self::PrepaintState {
        let mut frame = self
            .view
            .update(cx, |view, cx| view.prepare_frame(bounds, window, cx));
        frame.hitbox = Some(window.insert_hitbox(bounds, HitboxBehavior::Normal));
        frame
    }

    fn paint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        _request_layout: &mut Self::RequestLayoutState,
        frame: &mut Self::PrepaintState,
        window: &mut Window,
        cx: &mut App,
    ) {
        window.handle_input(
            &frame.focus_handle,
            ElementInputHandler::new(bounds, self.view.clone()),
            cx,
        );
        if let Some(hitbox) = frame.hitbox.take() {
            self.register_mouse_listeners(&hitbox, window, cx);
        }
        window.with_content_mask(Some(ContentMask { bounds }), |window| {
            window.paint_quad(fill(bounds, frame.background));
            for quad in frame.quads.drain(..) {
                window.paint_quad(quad);
            }
            for (origin, line) in &frame.lines {
                if let Err(err) = line.paint(
                    *origin,
                    frame.line_height,
                    TextAlign::Left,
                    None,
                    window,
                    cx,
                ) {
                    tracing::warn!(%err, "failed to paint terminal line");
                }
            }
            for quad in frame.overlays.drain(..) {
                window.paint_quad(quad);
            }
        });
        let bytes = frame.bytes_in_frame;
        self.view.update(cx, |view, _| {
            if let Some(stats) = view.stats.as_mut() {
                stats.frame_painted(bytes);
            }
        });
    }
}

impl TerminalElement {
    fn register_mouse_listeners(&self, hitbox: &Hitbox, window: &mut Window, cx: &mut App) {
        let reports = self.view.read(cx).pointer_reports(window);
        let style = if reports {
            CursorStyle::Arrow
        } else {
            CursorStyle::IBeam
        };
        window.set_cursor_style(style, hitbox);

        let view = self.view.clone();
        let down_hitbox = hitbox.clone();
        window.on_mouse_event(move |event: &MouseDownEvent, phase, window, cx| {
            if phase == DispatchPhase::Bubble && down_hitbox.is_hovered(window) {
                view.update(cx, |view, cx| view.mouse_down(event, window, cx));
            }
        });
        let view = self.view.clone();
        let move_hitbox = hitbox.clone();
        window.on_mouse_event(move |event: &MouseMoveEvent, phase, window, cx| {
            if phase == DispatchPhase::Bubble {
                let hovered = move_hitbox.is_hovered(window);
                view.update(cx, |view, cx| view.mouse_move(event, hovered, cx));
            }
        });
        let view = self.view.clone();
        window.on_mouse_event(move |event: &MouseUpEvent, phase, _window, cx| {
            if phase == DispatchPhase::Bubble {
                view.update(cx, |view, cx| view.mouse_up(event, cx));
            }
        });
    }
}

fn hsla(c: Rgb) -> Hsla {
    rgb(c.to_hex()).into()
}

/// Style key of a text run; cells with equal keys are shaped together.
#[derive(Clone, Copy, PartialEq, Eq)]
struct RunStyle {
    fg: Rgb,
    bold: bool,
    italic: bool,
    underline: bool,
    undercurl: bool,
    strike: bool,
}

impl RunStyle {
    fn of(cell: &SnapshotCell) -> Self {
        Self {
            fg: cell.fg,
            bold: cell.flags.contains(CellFlags::BOLD),
            italic: cell.flags.contains(CellFlags::ITALIC),
            underline: cell.flags.contains(CellFlags::UNDERLINE),
            undercurl: cell.flags.contains(CellFlags::UNDERCURL),
            strike: cell.flags.contains(CellFlags::STRIKEOUT),
        }
    }

    fn decorated(self) -> bool {
        self.underline || self.strike
    }
}

struct Run {
    col: usize,
    text: String,
    style: RunStyle,
    blank: bool,
}

impl TerminalView {
    fn cell_size(&mut self, window: &Window) -> Size<Pixels> {
        if let Some(cell) = self.cell {
            return cell;
        }
        let text_system = window.text_system();
        let font_id = text_system.resolve_font(&self.font);
        let size = self.options.font_size;
        let width = text_system
            .advance(font_id, size, 'm')
            .map(|advance| advance.width)
            .unwrap_or(size * 0.6);
        let cell = Size {
            width,
            height: (size * self.options.line_height).round(),
        };
        self.cell = Some(cell);
        cell
    }

    pub(super) fn prepare_frame(
        &mut self,
        bounds: Bounds<Pixels>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Frame {
        let cell = self.cell_size(window);
        self.grid_origin = bounds.origin;
        let cols = (f32::from(bounds.size.width) / f32::from(cell.width))
            .floor()
            .max(2.0);
        let rows = (f32::from(bounds.size.height) / f32::from(cell.height))
            .floor()
            .max(1.0);
        if let Ok(grid) = GridSize::new(cols as u16, rows as u16)
            && grid != self.grid
        {
            let cell_px = (f32::from(cell.width) as u16, f32::from(cell.height) as u16);
            match self.session.resize(grid, cell_px) {
                Ok(()) => self.grid = grid,
                Err(err) => tracing::warn!(%err, "failed to resize terminal"),
            }
        }

        let (mut snapshot, title, clipboard, bytes_in_frame) = {
            let mut terminal = self.session.terminal();
            let bytes = self.session.stats().bytes_read();
            (
                terminal.snapshot(),
                terminal.title(),
                terminal.take_clipboard_store(),
                bytes,
            )
        };
        if title != self.title {
            window.set_window_title(title.as_deref().unwrap_or("Hangar"));
            self.title = title;
        }
        if let Some(text) = clipboard {
            cx.write_to_clipboard(ClipboardItem::new_string(text));
        }

        let focused = self.focus_handle.is_focused(window);
        self.sync_focus(
            focused && window.is_window_active(),
            snapshot.modes.focus_reporting,
        );
        let mut overlays = Vec::new();
        self.cursor_bounds = None;
        if let Some(cursor) = snapshot.cursor
            && cursor.row < snapshot.rows
            && cursor.col < snapshot.cols
        {
            let index = cursor.row * snapshot.cols + cursor.col;
            let wide = snapshot.cells[index].flags.contains(CellFlags::WIDE);
            let width = if wide { cell.width * 2. } else { cell.width };
            let origin = bounds.origin
                + point(
                    cell.width * cursor.col as f32,
                    cell.height * cursor.row as f32,
                );
            let cell_bounds = Bounds::new(origin, size(width, cell.height));
            self.cursor_bounds = Some(cell_bounds);
            let color = hsla(snapshot.cursor_color);
            match (cursor.shape, focused) {
                (CursorShape::Block, true) => {
                    invert_for_cursor(&mut snapshot, index, wide);
                }
                (CursorShape::Block | CursorShape::HollowBlock, _) => {
                    overlays.push(outline(cell_bounds, color, BorderStyle::Solid));
                }
                (CursorShape::Beam, _) => {
                    overlays.push(fill(Bounds::new(origin, size(px(2.), cell.height)), color));
                }
                (CursorShape::Underline, _) => {
                    overlays.push(fill(
                        Bounds::new(
                            origin + point(px(0.), cell.height - px(2.)),
                            size(width, px(2.)),
                        ),
                        color,
                    ));
                }
            }
        }

        let mut selection = hsla(snapshot.foreground);
        selection.a = 0.3;
        let mut quads = Vec::new();
        let mut lines = Vec::new();
        for row in 0..snapshot.rows {
            let cells = snapshot.row(row);
            let y = bounds.origin.y + cell.height * row as f32;
            push_backgrounds(
                cells,
                snapshot.background,
                bounds.origin.x,
                y,
                cell,
                &mut quads,
            );
            push_selection(cells, bounds.origin.x, y, cell, selection, &mut quads);
            for run in text_runs(cells) {
                if run.blank && !run.style.decorated() {
                    continue;
                }
                let shaped = self.shape_run(&run, cell, window);
                let x = bounds.origin.x + cell.width * run.col as f32;
                lines.push((point(x, y), shaped));
            }
        }

        if let Some(marked) = self.marked_text.clone()
            && let Some(cursor) = self.cursor_bounds
        {
            let style = RunStyle {
                fg: snapshot.foreground,
                bold: false,
                italic: false,
                underline: true,
                undercurl: false,
                strike: false,
            };
            let run = Run {
                col: 0,
                text: marked,
                style,
                blank: false,
            };
            let shaped = self.shape_run(&run, cell, window);
            // Backgrounds go under the text: overlays paint after it and would hide it.
            quads.push(fill(
                Bounds::new(cursor.origin, size(shaped.width(), cell.height)),
                hsla(snapshot.background),
            ));
            lines.push((cursor.origin, shaped));
        }

        if let Some(message) = self.exit_message.clone() {
            let run = Run {
                col: 0,
                text: message,
                style: RunStyle {
                    fg: snapshot.background,
                    bold: true,
                    italic: false,
                    underline: false,
                    undercurl: false,
                    strike: false,
                },
                blank: false,
            };
            let shaped = self.shape_run(&run, cell, window);
            let origin = point(
                bounds.right() - shaped.width() - cell.width,
                bounds.bottom() - cell.height,
            );
            quads.push(fill(
                Bounds::new(origin, size(shaped.width(), cell.height)),
                hsla(snapshot.foreground),
            ));
            lines.push((origin, shaped));
        }

        Frame {
            focus_handle: self.focus_handle.clone(),
            hitbox: None,
            background: hsla(snapshot.background),
            quads,
            lines,
            overlays,
            line_height: cell.height,
            bytes_in_frame,
        }
    }

    fn shape_run(&self, run: &Run, cell: Size<Pixels>, window: &mut Window) -> ShapedLine {
        let mut font = self.font.clone();
        if run.style.bold {
            font.weight = FontWeight::BOLD;
        }
        if run.style.italic {
            font.style = FontStyle::Italic;
        }
        let color = hsla(run.style.fg);
        let text_run = TextRun {
            len: run.text.len(),
            font,
            color,
            background_color: None,
            underline: run.style.underline.then_some(UnderlineStyle {
                thickness: px(1.),
                color: Some(color),
                wavy: run.style.undercurl,
            }),
            strikethrough: run.style.strike.then_some(StrikethroughStyle {
                thickness: px(1.),
                color: Some(color),
            }),
        };
        window.text_system().shape_line(
            SharedString::from(run.text.clone()),
            self.options.font_size,
            &[text_run],
            Some(cell.width),
        )
    }
}

/// Draws the cell under a focused block cursor in inverted colours.
fn invert_for_cursor(snapshot: &mut Snapshot, index: usize, wide: bool) {
    let cursor_color = snapshot.cursor_color;
    let text_color = snapshot.background;
    let end = if wide { index + 2 } else { index + 1 };
    for cell in snapshot.cells.iter_mut().take(end).skip(index) {
        cell.bg = cursor_color;
        cell.fg = text_color;
    }
}

fn push_backgrounds(
    cells: &[SnapshotCell],
    default_bg: Rgb,
    x0: Pixels,
    y: Pixels,
    cell: Size<Pixels>,
    quads: &mut Vec<PaintQuad>,
) {
    let mut col = 0;
    while col < cells.len() {
        let bg = cells[col].bg;
        let start = col;
        while col < cells.len() && cells[col].bg == bg {
            col += 1;
        }
        if bg != default_bg {
            let origin = point(x0 + cell.width * start as f32, y);
            let extent = size(cell.width * (col - start) as f32, cell.height);
            quads.push(fill(Bounds::new(origin, extent), hsla(bg)));
        }
    }
}

/// Translucent highlight over selected cells, one quad per contiguous run.
fn push_selection(
    cells: &[SnapshotCell],
    x0: Pixels,
    y: Pixels,
    cell: Size<Pixels>,
    color: Hsla,
    quads: &mut Vec<PaintQuad>,
) {
    let mut col = 0;
    while col < cells.len() {
        if !cells[col].flags.contains(CellFlags::SELECTED) {
            col += 1;
            continue;
        }
        let start = col;
        while col < cells.len() && cells[col].flags.contains(CellFlags::SELECTED) {
            col += 1;
        }
        let origin = point(x0 + cell.width * start as f32, y);
        let extent = size(cell.width * (col - start) as f32, cell.height);
        quads.push(fill(Bounds::new(origin, extent), color));
    }
}

/// Splits a row into runs of equal style. Wide glyphs get their own run because
/// `force_width` assumes one cell per glyph.
fn text_runs(cells: &[SnapshotCell]) -> Vec<Run> {
    let mut runs: Vec<Run> = Vec::new();
    let mut current: Option<Run> = None;
    for (col, cell) in cells.iter().enumerate() {
        if cell.flags.contains(CellFlags::WIDE_SPACER) {
            runs.extend(current.take());
            continue;
        }
        let style = RunStyle::of(cell);
        let wide = cell.flags.contains(CellFlags::WIDE);
        let continues = current.as_ref().is_some_and(|run| run.style == style) && !wide;
        if !continues {
            runs.extend(current.take());
            current = Some(Run {
                col,
                text: String::new(),
                style,
                blank: true,
            });
        }
        if let Some(run) = current.as_mut() {
            let ch = if cell.ch == '\t' { ' ' } else { cell.ch };
            run.text.push(ch);
            run.text.extend(cell.zerowidth.iter());
            run.blank &= ch == ' ';
        }
        if wide {
            runs.extend(current.take());
        }
    }
    runs.extend(current);
    runs
}

#[cfg(test)]
mod tests {
    use super::text_runs;
    use hangar_terminal::{CellFlags, Rgb, SnapshotCell};

    fn cell(ch: char, fg: u32, flags: CellFlags) -> SnapshotCell {
        SnapshotCell {
            ch,
            zerowidth: Vec::new(),
            fg: Rgb::from_hex(fg),
            bg: Rgb::from_hex(0),
            flags,
        }
    }

    #[test]
    fn runs_split_on_style_and_wide_glyphs() {
        let plain = CellFlags::empty();
        let cells = vec![
            cell('a', 1, plain),
            cell('b', 1, plain),
            cell('c', 2, plain),
            cell('日', 2, CellFlags::WIDE),
            cell(' ', 2, CellFlags::WIDE_SPACER),
            cell('d', 2, plain),
            cell(' ', 2, plain),
        ];
        let runs = text_runs(&cells);
        let summary: Vec<(usize, &str, bool)> = runs
            .iter()
            .map(|r| (r.col, r.text.as_str(), r.blank))
            .collect();
        assert_eq!(
            summary,
            vec![
                (0, "ab", false),
                (2, "c", false),
                (3, "日", false),
                (5, "d ", false)
            ]
        );
    }

    #[test]
    fn blank_runs_are_marked() {
        let cells = vec![
            cell(' ', 1, CellFlags::empty()),
            cell(' ', 1, CellFlags::empty()),
        ];
        let runs = text_runs(&cells);
        assert_eq!(runs.len(), 1);
        assert!(runs[0].blank);
    }
}
