//! Live device screen: paints the latest frame of a [`DeviceSession`] aspect-fit in its bounds
//! and turns mouse presses into touch input (click → tap, drag → swipe, hold → long press).
//!
//! Frames are converted to a `RenderImage` on the background executor; the previous image is
//! released from the sprite atlas once a newer one has been painted.

pub mod geometry;
mod session;

use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::sync::Arc;
use std::time::{Duration, Instant};

use gpui_kit::component::ActiveTheme as _;
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use hangar_devices::{DeviceInfo, Frame, Input, Orientation};

use geometry::{Gesture, Rect};
use session::Outcome;
pub use session::{DeviceSession, InputResult, LogSource, TextResult};

pub enum DeviceViewEvent {
    /// An input was delivered (or failed). Successful `TouchMove`s are not reported.
    Input(InputResult),
    /// Typed text could not be delivered.
    Text(TextResult),
    /// The device was asked to shut down (see [`DeviceView::shutdown`]).
    Shutdown(Result<(), String>),
    /// A rotation finished: the orientation asked for, and whether the device followed.
    Rotate(Orientation, Result<(), String>),
}

struct Shown {
    image: Arc<RenderImage>,
    frame: Frame,
    /// Set once the image has been painted (and so uploaded to the atlas).
    painted: Rc<Cell<bool>>,
}

pub struct DeviceView {
    session: DeviceSession,
    shown: Option<Shown>,
    /// Painted images replaced by newer ones; freed from the atlas on the next render.
    retired: Vec<Arc<RenderImage>>,
    /// Where the current frame was last painted, in window coordinates.
    image_rect: Rc<Cell<Option<Rect>>>,
    /// Orientation the device was last rotated to (starts from the first frame's shape).
    orientation: Orientation,
    gesture: Gesture,
    press: u64,
    hold: Option<Task<()>>,
    stats: Rc<RefCell<Stats>>,
    focus_handle: FocusHandle,
    _frames: Task<()>,
    _results: Task<()>,
}

impl EventEmitter<DeviceViewEvent> for DeviceView {}

impl DeviceView {
    pub fn new(mut session: DeviceSession, stats: bool, cx: &mut Context<Self>) -> Self {
        let frames = cx.spawn({
            let mut rx = session.frames();
            async move |this, cx| {
                loop {
                    if rx.changed().await.is_err() {
                        break;
                    }
                    let Some(frame) = rx.borrow_and_update().clone() else {
                        continue;
                    };
                    let built = cx
                        .background_executor()
                        .spawn(async move { to_render_image(&frame).map(|img| (img, frame)) })
                        .await;
                    let Some((image, frame)) = built else {
                        tracing::warn!("device frame has an invalid layout; skipped");
                        continue;
                    };
                    let alive = this.update(cx, |view, cx| view.show(image, frame, cx));
                    if alive.is_err() {
                        break;
                    }
                }
            }
        });
        let results = match session.take_results() {
            Some(mut rx) => cx.spawn(async move |this, cx| {
                while let Some(outcome) = rx.recv().await {
                    let event = match outcome {
                        Outcome::Input(result) => DeviceViewEvent::Input(result),
                        Outcome::Text(result) => DeviceViewEvent::Text(result),
                        Outcome::Shutdown(result) => DeviceViewEvent::Shutdown(result),
                        Outcome::Rotate(orientation, result) => {
                            DeviceViewEvent::Rotate(orientation, result)
                        }
                    };
                    let updated = this.update(cx, |view, cx| {
                        if let DeviceViewEvent::Rotate(orientation, Ok(())) = &event {
                            view.orientation = *orientation;
                        }
                        cx.emit(event);
                    });
                    if updated.is_err() {
                        break;
                    }
                }
            }),
            None => Task::ready(()),
        };
        Self {
            session,
            shown: None,
            retired: Vec::new(),
            image_rect: Rc::new(Cell::new(None)),
            orientation: Orientation::Portrait,
            gesture: Gesture::default(),
            press: 0,
            hold: None,
            stats: Rc::new(RefCell::new(Stats::new(stats))),
            focus_handle: cx.focus_handle(),
            _frames: frames,
            _results: results,
        }
    }

    pub fn info(&self) -> &DeviceInfo {
        self.session.info()
    }

    pub fn backend(&self) -> &'static str {
        self.session.backend()
    }

    pub fn log_source(&self) -> LogSource {
        self.session.log_source()
    }

    /// Queues input for the device (hardware buttons, dev hooks).
    pub fn send(&self, input: Input) {
        if !self.session.send(input) {
            tracing::warn!(?input, "device input thread has stopped");
        }
    }

    /// Asks the device to shut down (in the background); the result arrives as an event.
    pub fn shutdown(&self) {
        if !self.session.shutdown() {
            tracing::warn!("device input thread has stopped");
        }
    }

    /// Turns the device one step left (counter-clockwise) or right, cycling through portrait and
    /// the two landscapes (upside-down is skipped: Android leaves it disabled on phones). The
    /// outcome arrives as [`DeviceViewEvent::Rotate`]; frames and touch mapping follow.
    pub fn rotate(&self, left: bool) {
        let current = match &self.shown {
            // A device rotated elsewhere: trust the frame shape when it disagrees.
            Some(s) if (s.frame.width > s.frame.height) != self.orientation.is_landscape() => {
                if s.frame.width > s.frame.height {
                    Orientation::LandscapeLeft
                } else {
                    Orientation::Portrait
                }
            }
            _ => self.orientation,
        };
        let mut next = current.rotated(left);
        if next == Orientation::PortraitUpsideDown {
            next = next.rotated(left);
        }
        if !self.session.rotate(next) {
            tracing::warn!("device input thread has stopped");
        }
    }

    /// Queues typed text for the focused field of the device.
    pub fn send_text(&self, text: String) {
        if !self.session.send_text(text) {
            tracing::warn!("device input thread has stopped");
        }
    }

    fn on_key_down(&mut self, event: &KeyDownEvent, _: &mut Window, cx: &mut Context<Self>) {
        if let Some(text) = key_text(&event.keystroke) {
            self.send_text(text);
            cx.stop_propagation();
        }
    }

    pub fn stats_enabled(&self) -> bool {
        self.stats.borrow().enabled
    }

    pub fn set_stats(&mut self, enabled: bool, cx: &mut Context<Self>) {
        self.stats.borrow_mut().set_enabled(enabled);
        cx.notify();
    }

    /// The latest stats line (once a second while stats are enabled and frames are painted).
    pub fn stats_report(&self) -> Option<String> {
        self.stats.borrow().report.clone()
    }

    /// The frame currently on screen, once it has actually been painted.
    pub fn painted_frame(&self) -> Option<&Frame> {
        self.shown
            .as_ref()
            .filter(|s| s.painted.get())
            .map(|s| &s.frame)
    }

    fn show(&mut self, image: Arc<RenderImage>, frame: Frame, cx: &mut Context<Self>) {
        self.stats.borrow_mut().received();
        if let Some(old) = self.shown.take() {
            // An image that never reached the atlas (window hidden) has nothing to free.
            if old.painted.get() {
                self.retired.push(old.image);
            }
        }
        self.shown = Some(Shown {
            image,
            frame,
            painted: Rc::new(Cell::new(false)),
        });
        cx.notify();
    }

    fn screen_points(&self) -> Option<(f64, f64)> {
        geometry::screen_points(
            self.session.info().screen,
            self.shown.as_ref().map(|s| (s.frame.width, s.frame.height)),
        )
    }

    fn device_point(&self, position: Point<Pixels>, clamped: bool) -> Option<(f64, f64)> {
        let image = self.image_rect.get()?;
        let screen = self.screen_points()?;
        let (x, y) = (f64::from(position.x), f64::from(position.y));
        if clamped {
            geometry::to_device_clamped(x, y, image, screen)
        } else {
            geometry::to_device(x, y, image, screen)
        }
    }

    fn send_all(&self, inputs: Vec<Input>) {
        for input in inputs {
            self.send(input);
        }
    }

    fn mouse_down(&mut self, position: Point<Pixels>, cx: &mut Context<Self>) {
        let device = self.device_point(position, false);
        let view = (f64::from(position.x), f64::from(position.y));
        let inputs = self.gesture.press(view, device);
        self.send_all(inputs);
        if !self.gesture.is_pressed() {
            return;
        }
        self.press += 1;
        let press = self.press;
        self.hold = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(geometry::LONG_PRESS).await;
            let _ = this.update(cx, |view, _| {
                if view.press == press {
                    let inputs = view.gesture.hold_elapsed();
                    view.send_all(inputs);
                }
            });
        }));
    }

    fn mouse_move(&mut self, position: Point<Pixels>) {
        if self.gesture == Gesture::Idle {
            return;
        }
        let Some(device) = self.device_point(position, true) else {
            return;
        };
        let view = (f64::from(position.x), f64::from(position.y));
        let inputs = self.gesture.moved(view, device);
        self.send_all(inputs);
    }

    fn mouse_up(&mut self, position: Point<Pixels>) {
        if self.gesture == Gesture::Idle {
            return;
        }
        self.hold = None;
        let device = self.device_point(position, true);
        let inputs = self.gesture.release(device);
        self.send_all(inputs);
    }
}

impl Drop for DeviceView {
    fn drop(&mut self) {
        // Never leave a finger down on the device.
        let inputs = self.gesture.cancel();
        self.send_all(inputs);
    }
}

/// The text a key press types on the device. Shortcuts (Cmd, Ctrl) and keys without text
/// (arrows, Escape) are not typed.
fn key_text(keystroke: &Keystroke) -> Option<String> {
    let m = keystroke.modifiers;
    if m.platform || m.control || m.function {
        return None;
    }
    match keystroke.key.as_str() {
        "enter" => Some("\n".to_owned()),
        "backspace" => Some("\u{8}".to_owned()),
        _ => keystroke
            .key_char
            .clone()
            .filter(|text| !text.is_empty() && !text.chars().any(char::is_control)),
    }
}

impl Focusable for DeviceView {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

/// Copies a BGRA frame into a GPUI image (which keeps BGRA bytes in an `RgbaImage` container).
fn to_render_image(frame: &Frame) -> Option<Arc<RenderImage>> {
    let row = frame.width as usize * 4;
    let mut data = Vec::with_capacity(row * frame.height as usize);
    for y in 0..frame.height as usize {
        let start = y * frame.stride;
        data.extend_from_slice(frame.bgra.get(start..start + row)?);
    }
    let buffer = image::RgbaImage::from_raw(frame.width, frame.height, data)?;
    Some(Arc::new(RenderImage::new(vec![image::Frame::new(buffer)])))
}

impl Render for DeviceView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        for old in self.retired.drain(..) {
            let _ = window.drop_image(old);
        }
        let view = cx.entity();
        let shown = self.shown.as_ref().map(|s| {
            (
                s.image.clone(),
                s.painted.clone(),
                s.frame.seq,
                s.frame.captured_at,
                (s.frame.width, s.frame.height),
            )
        });
        let has_frame = shown.is_some();
        let image_rect = self.image_rect.clone();
        let stats = self.stats.clone();
        let screen = canvas(
            |bounds, window, _| window.insert_hitbox(bounds, HitboxBehavior::Normal),
            move |bounds, hitbox, window, _| {
                window.set_cursor_style(CursorStyle::PointingHand, &hitbox);
                register_mouse_listeners(view, hitbox, window);
                let Some((image, painted, seq, captured_at, (w, h))) = shown else {
                    image_rect.set(None);
                    return;
                };
                let rect = geometry::image_rect(
                    Rect {
                        x: f64::from(bounds.origin.x),
                        y: f64::from(bounds.origin.y),
                        width: f64::from(bounds.size.width),
                        height: f64::from(bounds.size.height),
                    },
                    f64::from(w),
                    f64::from(h),
                    f64::from(window.scale_factor()),
                );
                image_rect.set(Some(rect));
                if rect.is_empty() {
                    return;
                }
                let target = Bounds::new(
                    point(px(rect.x as f32), px(rect.y as f32)),
                    size(px(rect.width as f32), px(rect.height as f32)),
                );
                if let Err(err) =
                    window.paint_image(target, target, Corners::default(), image, 0, false)
                {
                    tracing::warn!(%err, "failed to paint device frame");
                    return;
                }
                painted.set(true);
                stats.borrow_mut().painted(seq, captured_at, (w, h));
            },
        )
        .size_full();

        let overlay = self.stats.borrow().overlay();
        let theme = cx.theme();
        div()
            .id("device-screen")
            .relative()
            .size_full()
            .track_focus(&self.focus_handle)
            .on_key_down(cx.listener(Self::on_key_down))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|view, _: &MouseDownEvent, window, cx| {
                    window.focus(&view.focus_handle, cx);
                }),
            )
            .child(screen)
            .when(!has_frame, |this| {
                this.child(
                    div()
                        .absolute()
                        .inset_0()
                        .flex()
                        .items_center()
                        .justify_center()
                        .text_xs()
                        .text_color(theme.muted_foreground)
                        .child("Waiting for the first frame…"),
                )
            })
            .when_some(overlay, |this, text| {
                this.child(
                    div()
                        .absolute()
                        .top_1()
                        .left_1()
                        .px_1()
                        .rounded_sm()
                        .bg(black().opacity(0.6))
                        .text_color(white())
                        .text_xs()
                        .font_family("Menlo")
                        .child(text),
                )
            })
    }
}

fn register_mouse_listeners(view: Entity<DeviceView>, hitbox: Hitbox, window: &mut Window) {
    let down = view.clone();
    window.on_mouse_event(move |event: &MouseDownEvent, phase, window, cx| {
        if phase == DispatchPhase::Bubble
            && event.button == MouseButton::Left
            && hitbox.is_hovered(window)
        {
            down.update(cx, |view, cx| view.mouse_down(event.position, cx));
        }
    });
    // Moves and releases are tracked outside the bounds too, so a drag can leave the screen.
    let moved = view.clone();
    window.on_mouse_event(move |event: &MouseMoveEvent, phase, _, cx| {
        if phase == DispatchPhase::Bubble && event.dragging() {
            moved.update(cx, |view, _| view.mouse_move(event.position));
        }
    });
    window.on_mouse_event(move |event: &MouseUpEvent, phase, _, cx| {
        if phase == DispatchPhase::Bubble && event.button == MouseButton::Left {
            view.update(cx, |view, _| view.mouse_up(event.position));
        }
    });
}

/// Frames received/painted per second and capture → paint latency, reported once a second.
struct Stats {
    enabled: bool,
    first_painted: bool,
    last_painted_seq: u64,
    window_start: Instant,
    received: u32,
    painted: u32,
    latency_sum: Duration,
    latency_max: Duration,
    report: Option<String>,
}

impl Stats {
    fn new(enabled: bool) -> Self {
        Self {
            enabled,
            first_painted: false,
            last_painted_seq: 0,
            window_start: Instant::now(),
            received: 0,
            painted: 0,
            latency_sum: Duration::ZERO,
            latency_max: Duration::ZERO,
            report: None,
        }
    }

    fn set_enabled(&mut self, enabled: bool) {
        if enabled != self.enabled {
            *self = Self {
                first_painted: self.first_painted,
                last_painted_seq: self.last_painted_seq,
                ..Self::new(enabled)
            };
        }
    }

    fn received(&mut self) {
        if self.enabled {
            self.received += 1;
        }
    }

    /// Counts each frame once: re-renders of an already painted frame are not new paints.
    fn painted(&mut self, seq: u64, captured_at: Instant, size: (u32, u32)) {
        if seq == self.last_painted_seq && self.first_painted {
            return;
        }
        self.last_painted_seq = seq;
        let latency = captured_at.elapsed();
        if !self.first_painted {
            self.first_painted = true;
            tracing::info!(
                seq,
                width = size.0,
                height = size.1,
                capture_to_paint_ms = latency.as_secs_f64() * 1000.0,
                "device: first frame painted"
            );
        }
        if !self.enabled {
            return;
        }
        self.painted += 1;
        self.latency_sum += latency;
        self.latency_max = self.latency_max.max(latency);
        self.maybe_report();
    }

    fn maybe_report(&mut self) {
        let elapsed = self.window_start.elapsed();
        if elapsed < Duration::from_secs(1) {
            return;
        }
        let secs = elapsed.as_secs_f64();
        let avg = if self.painted > 0 {
            self.latency_sum.as_secs_f64() * 1000.0 / f64::from(self.painted)
        } else {
            0.0
        };
        let report = format!(
            "recv {:.0} fps · paint {:.0} fps · capture→paint {:.1} ms (max {:.1})",
            f64::from(self.received) / secs,
            f64::from(self.painted) / secs,
            avg,
            self.latency_max.as_secs_f64() * 1000.0
        );
        tracing::info!("[device-stats] {report}");
        self.report = Some(report);
        self.window_start = Instant::now();
        self.received = 0;
        self.painted = 0;
        self.latency_sum = Duration::ZERO;
        self.latency_max = Duration::ZERO;
    }

    fn overlay(&self) -> Option<String> {
        self.enabled.then(|| {
            self.report
                .clone()
                .unwrap_or_else(|| "measuring… (stats update on new frames)".to_owned())
        })
    }
}

#[cfg(test)]
mod tests {
    use super::key_text;
    use gpui_kit::{Keystroke, Modifiers};

    fn key(key: &str, key_char: Option<&str>, modifiers: Modifiers) -> Keystroke {
        Keystroke {
            modifiers,
            key: key.to_owned(),
            key_char: key_char.map(str::to_owned),
        }
    }

    #[test]
    fn typed_keys_become_text() {
        let none = Modifiers::default();
        let shift = Modifiers::shift();
        assert_eq!(key_text(&key("a", Some("a"), none)).as_deref(), Some("a"));
        assert_eq!(key_text(&key("a", Some("A"), shift)).as_deref(), Some("A"));
        assert_eq!(
            key_text(&key("space", Some(" "), none)).as_deref(),
            Some(" ")
        );
        assert_eq!(
            key_text(&key("enter", Some("\n"), none)).as_deref(),
            Some("\n")
        );
        assert_eq!(
            key_text(&key("backspace", None, none)).as_deref(),
            Some("\u{8}")
        );
    }

    #[test]
    fn shortcuts_and_textless_keys_are_not_typed() {
        assert_eq!(key_text(&key("v", Some("v"), Modifiers::command())), None);
        assert_eq!(key_text(&key("c", Some("c"), Modifiers::control())), None);
        assert_eq!(key_text(&key("left", None, Modifiers::default())), None);
        assert_eq!(
            key_text(&key("escape", Some("\u{1b}"), Modifiers::default())),
            None
        );
    }
}
