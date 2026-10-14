//! Pointer input: text selection, or reports to programs that enabled mouse tracking.
//! Handlers never stop propagation: enclosing panes use the same clicks to take focus.

use std::time::Duration;

use gpui_kit::*;
use hangar_terminal::{
    GridSize, MouseAction, MouseReport, SelectionKind, Side, ViewportPoint, encode_focus,
    encode_mouse,
};

use super::TerminalView;

/// How often the viewport scrolls while a selection is dragged past the top or bottom edge.
const AUTOSCROLL_INTERVAL: Duration = Duration::from_millis(50);
/// Upper bound of lines scrolled per autoscroll step, reached far from the edge.
const AUTOSCROLL_MAX_LINES: i32 = 8;

#[derive(Default)]
pub(super) struct MouseState {
    /// A left-button selection drag is in progress.
    selecting: bool,
    /// Last pointer position of the selection drag, used by autoscroll.
    drag_position: Point<Pixels>,
    autoscroll: Option<Task<()>>,
    /// Button pressed and reported to the program, until its release is reported.
    reported_button: Option<hangar_terminal::MouseButton>,
    /// Cell of the last reported motion; motion is reported once per cell.
    reported_cell: Option<ViewportPoint>,
}

impl TerminalView {
    /// Cell under a window position, clamped to the grid, and which half of it.
    fn cell_at(&self, position: Point<Pixels>) -> Option<(ViewportPoint, Side)> {
        let cell = self.cell?;
        Some(cell_at(position - self.grid_origin, cell, self.grid))
    }

    /// Lines to scroll for a drag at `position`: positive above the grid, negative below.
    fn autoscroll_lines(&self, position: Point<Pixels>) -> i32 {
        let Some(cell) = self.cell else { return 0 };
        let top = self.grid_origin.y;
        let bottom = top + cell.height * f32::from(self.grid.rows);
        let distance = if position.y < top {
            top - position.y
        } else if position.y > bottom {
            bottom - position.y
        } else {
            return 0;
        };
        let lines = 1 + (f32::from(distance.abs()) / f32::from(cell.height)) as i32;
        lines.min(AUTOSCROLL_MAX_LINES) * if distance > px(0.) { 1 } else { -1 }
    }

    fn mouse_tracking_applies(&self, modifiers: &Modifiers) -> bool {
        !modifiers.shift && self.session.terminal().mouse_mode().is_active()
    }

    pub(super) fn mouse_down(
        &mut self,
        event: &MouseDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        window.focus(&self.focus_handle, cx);
        let Some((point, side)) = self.cell_at(event.position) else {
            return;
        };
        if self.mouse_tracking_applies(&event.modifiers) {
            if let Some(button) = report_button(event.button) {
                self.report(MouseAction::Press(button), point, &event.modifiers);
                self.mouse.reported_button = Some(button);
                self.mouse.reported_cell = Some(point);
            }
            return;
        }
        if event.button == MouseButton::Middle {
            self.paste_selection_or_clipboard(cx);
            return;
        }
        if event.button != MouseButton::Left {
            return;
        }
        {
            let mut terminal = self.session.terminal();
            if event.modifiers.shift {
                terminal.update_selection(point, side);
            } else {
                let kind = match event.click_count {
                    // Option-drag selects a rectangle, as in Terminal.app.
                    0 | 1 if event.modifiers.alt => SelectionKind::Block,
                    0 | 1 => SelectionKind::Simple,
                    2 => SelectionKind::Word,
                    _ => SelectionKind::Line,
                };
                terminal.start_selection(kind, point, side);
            }
        }
        self.mouse.selecting = true;
        self.mouse.drag_position = event.position;
        cx.notify();
    }

    pub(super) fn mouse_move(
        &mut self,
        event: &MouseMoveEvent,
        hovered: bool,
        cx: &mut Context<Self>,
    ) {
        if self.mouse.selecting {
            if event.pressed_button != Some(MouseButton::Left) {
                // The release happened where we could not see it.
                self.finish_selection(cx);
                return;
            }
            self.mouse.drag_position = event.position;
            self.drag_selection(cx);
            self.update_autoscroll(cx);
            return;
        }

        let held = self.mouse.reported_button;
        if held.is_none() && (!hovered || event.modifiers.shift) {
            return;
        }
        let Some((point, _)) = self.cell_at(event.position) else {
            return;
        };
        if self.mouse.reported_cell == Some(point) {
            return;
        }
        if self.report(MouseAction::Move(held), point, &event.modifiers) {
            self.mouse.reported_cell = Some(point);
        }
    }

    pub(super) fn mouse_up(&mut self, event: &MouseUpEvent, cx: &mut Context<Self>) {
        if self.mouse.selecting && event.button == MouseButton::Left {
            self.mouse.drag_position = event.position;
            self.drag_selection(cx);
            self.finish_selection(cx);
            return;
        }
        let Some(held) = self.mouse.reported_button else {
            return;
        };
        if report_button(event.button) != Some(held) {
            return;
        }
        if let Some((point, _)) = self.cell_at(event.position) {
            self.report(MouseAction::Release(held), point, &event.modifiers);
        }
        self.mouse.reported_button = None;
        self.mouse.reported_cell = None;
    }

    /// Sends wheel steps to the program at the pointer's cell; positive `lines` is up.
    pub(super) fn report_wheel(&mut self, event: &ScrollWheelEvent, lines: i32) {
        let Some((point, _)) = self.cell_at(event.position) else {
            return;
        };
        let button = if lines > 0 {
            hangar_terminal::MouseButton::WheelUp
        } else {
            hangar_terminal::MouseButton::WheelDown
        };
        for _ in 0..lines.unsigned_abs() {
            self.report(MouseAction::Press(button), point, &event.modifiers);
        }
    }

    /// Reports focus changes (view focused and window active) when DECSET 1004 is on.
    /// Called at paint, which GPUI triggers on focus and window activation changes.
    pub(super) fn sync_focus(&mut self, focused: bool, reporting: bool) {
        if self.reported_focus == Some(focused) {
            return;
        }
        if self.reported_focus.is_some() && reporting {
            self.send_report(encode_focus(focused));
        }
        self.reported_focus = Some(focused);
    }

    /// Whether the program receives pointer events (selection is unavailable without Shift).
    pub(super) fn pointer_reports(&self, window: &Window) -> bool {
        !window.modifiers().shift && self.session.terminal().mouse_mode().is_active()
    }

    /// Encodes and sends a report under the current mode; false when the mode ignores it.
    fn report(&self, action: MouseAction, point: ViewportPoint, modifiers: &Modifiers) -> bool {
        let mode = self.session.terminal().mouse_mode();
        let report = MouseReport {
            action,
            col: point.col,
            row: point.row,
            mods: hangar_terminal::Modifiers {
                shift: modifiers.shift,
                alt: modifiers.alt,
                ctrl: modifiers.control,
            },
        };
        match encode_mouse(report, mode) {
            Some(bytes) => {
                self.send_report(&bytes);
                true
            }
            None => false,
        }
    }

    fn drag_selection(&mut self, cx: &mut Context<Self>) {
        if let Some((point, side)) = self.cell_at(self.mouse.drag_position) {
            self.session.terminal().update_selection(point, side);
            cx.notify();
        }
    }

    fn finish_selection(&mut self, cx: &mut Context<Self>) {
        self.mouse.selecting = false;
        self.mouse.autoscroll = None;
        let mut terminal = self.session.terminal();
        if !terminal.has_selection() {
            terminal.clear_selection();
        }
        cx.notify();
    }

    fn update_autoscroll(&mut self, cx: &mut Context<Self>) {
        if self.autoscroll_lines(self.mouse.drag_position) == 0 {
            self.mouse.autoscroll = None;
            return;
        }
        if self.mouse.autoscroll.is_some() {
            return;
        }
        self.mouse.autoscroll = Some(cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(AUTOSCROLL_INTERVAL).await;
                let scrolled = this.update(cx, |view, cx| {
                    let lines = view.autoscroll_lines(view.mouse.drag_position);
                    if !view.mouse.selecting || lines == 0 {
                        return false;
                    }
                    view.session.terminal().scroll(lines);
                    view.drag_selection(cx);
                    true
                });
                if !matches!(scrolled, Ok(true)) {
                    break;
                }
            }
        }));
    }
}

/// Maps a position relative to the grid's top-left corner to a cell, clamping positions
/// outside the grid to its edges.
fn cell_at(relative: Point<Pixels>, cell: Size<Pixels>, grid: GridSize) -> (ViewportPoint, Side) {
    let cols = usize::from(grid.cols);
    let rows = usize::from(grid.rows);
    let x = f32::from(relative.x) / f32::from(cell.width);
    let y = f32::from(relative.y) / f32::from(cell.height);
    let (col, side) = if x < 0. {
        (0, Side::Left)
    } else if x >= cols as f32 {
        (cols - 1, Side::Right)
    } else if x.fract() < 0.5 {
        (x as usize, Side::Left)
    } else {
        (x as usize, Side::Right)
    };
    let row = if y < 0. {
        0
    } else {
        (y as usize).min(rows - 1)
    };
    (ViewportPoint::new(row, col), side)
}

fn report_button(button: MouseButton) -> Option<hangar_terminal::MouseButton> {
    match button {
        MouseButton::Left => Some(hangar_terminal::MouseButton::Left),
        MouseButton::Middle => Some(hangar_terminal::MouseButton::Middle),
        MouseButton::Right => Some(hangar_terminal::MouseButton::Right),
        MouseButton::Navigate(_) => None,
    }
}

#[cfg(test)]
mod tests {
    use super::cell_at;
    use gpui_kit::{Size, point, px};
    use hangar_terminal::{GridSize, Side, ViewportPoint};

    #[test]
    fn positions_map_to_cells_and_halves() {
        let cell = Size {
            width: px(10.),
            height: px(20.),
        };
        let grid = GridSize::new(8, 4).unwrap();
        let at = |x: f32, y: f32| cell_at(point(px(x), px(y)), cell, grid);
        assert_eq!(at(0., 0.), (ViewportPoint::new(0, 0), Side::Left));
        assert_eq!(at(14., 25.), (ViewportPoint::new(1, 1), Side::Left));
        assert_eq!(at(16., 25.), (ViewportPoint::new(1, 1), Side::Right));
        // Outside the grid: clamped to the nearest edge cell.
        assert_eq!(at(-5., -5.), (ViewportPoint::new(0, 0), Side::Left));
        assert_eq!(at(500., 500.), (ViewportPoint::new(3, 7), Side::Right));
    }
}
