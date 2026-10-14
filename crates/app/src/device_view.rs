//! Live device screen: paints the latest frame of a [`DeviceSession`] aspect-fit in its bounds
//! and turns mouse presses into touch input (click → tap, drag → swipe, hold → long press).
//!
//! A live simulator surface is painted as is (`paint_surface`, zero-copy). Other frames are
//! converted to a `RenderImage` on the background executor; the previous image is released from
//! the sprite atlas once a newer one has been painted.

pub mod geometry;
mod session;
#[cfg(target_os = "macos")]
mod surface;

use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::sync::Arc;
use std::time::{Duration, Instant};

use gpui_kit::component::ActiveTheme as _;
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use hangar_devices::{DeviceInfo, Frame, FramePixels, Input, Key, Orientation};

use geometry::{Gesture, Rect};
pub use session::{DeviceHandle, DeviceSession, InputResult, Outcome, TextResult};

/// Key context of the device screen; bindings in it take precedence over app-wide ones.
pub const KEY_CONTEXT: &str = "DeviceScreen";

/// Registers key bindings the device screen needs. Call once after `gpui_kit::init`.
///
/// gpui-kit's Root binds Tab / Shift-Tab to focus navigation; on the device screen those keys
/// belong to the simulated device.
pub fn init(cx: &mut App) {
    cx.bind_keys([
        KeyBinding::new("tab", NoAction {}, Some(KEY_CONTEXT)),
        KeyBinding::new("shift-tab", NoAction {}, Some(KEY_CONTEXT)),
    ]);
}

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

#[derive(Clone)]
enum Picture {
    Image(Arc<RenderImage>),
    #[cfg(target_os = "macos")]
    Surface(core_video::pixel_buffer::CVPixelBuffer),
}

struct Shown {
    picture: Picture,
    frame: Frame,
    /// Set once the picture has been painted (an image is then in the atlas).
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
    #[cfg(target_os = "macos")]
    surfaces: surface::SurfaceCache,
    /// A live surface could not be painted directly and is copied instead (logged once).
    surface_fallback: bool,
    focus_handle: FocusHandle,
    _frames: Task<()>,
    _results: Task<()>,
}

impl EventEmitter<DeviceViewEvent> for DeviceView {}

impl DeviceView {
    pub fn new(session: DeviceSession, stats: bool, cx: &mut Context<Self>) -> Self {
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
                    let Ok(surface) = this.update(cx, |view, _| view.surface_picture(&frame))
                    else {
                        break;
                    };
                    let built = match surface {
                        Some(picture) => Some((picture, frame)),
                        None => cx
                            .background_executor()
                            .spawn(async move { to_render_image(&frame).map(|img| (img, frame)) })
                            .await
                            .map(|(img, frame)| (Picture::Image(img), frame)),
                    };
                    let Some((picture, frame)) = built else {
                        tracing::warn!("device frame has an invalid layout; skipped");
                        continue;
                    };
                    let alive = this.update(cx, |view, cx| view.show(picture, frame, cx));
                    if alive.is_err() {
                        break;
                    }
                }
            }
        });
        let results = {
            let mut rx = session.subscribe();
            cx.spawn(async move |this, cx| {
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
            })
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
            #[cfg(target_os = "macos")]
            surfaces: surface::SurfaceCache::default(),
            surface_fallback: false,
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

    pub fn handle(&self) -> DeviceHandle {
        self.session.handle()
    }

    pub fn session(&self) -> &DeviceSession {
        &self.session
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
        if let Some(key) = named_key(&event.keystroke) {
            self.send(Input::Key(key));
            cx.stop_propagation();
        } else if let Some(text) = key_text(&event.keystroke) {
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

    /// The zero-copy picture of a live frame; `None` means "convert it to an image".
    fn surface_picture(&mut self, frame: &Frame) -> Option<Picture> {
        let FramePixels::Live(surface) = &frame.pixels else {
            return None;
        };
        #[cfg(target_os = "macos")]
        if let Some(buffer) = self.surfaces.buffer(surface) {
            return Some(Picture::Surface(buffer));
        }
        if !self.surface_fallback {
            self.surface_fallback = true;
            tracing::warn!(
                ?surface,
                "live device surface cannot be painted directly; copying frames"
            );
        }
        None
    }

    fn show(&mut self, picture: Picture, frame: Frame, cx: &mut Context<Self>) {
        self.stats.borrow_mut().received();
        if let Some(old) = self.shown.take() {
            // An image that never reached the atlas (window hidden) has nothing to free.
            if let (Picture::Image(image), true) = (old.picture, old.painted.get()) {
                self.retired.push(image);
            }
        }
        self.shown = Some(Shown {
            picture,
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

fn is_shortcut(keystroke: &Keystroke) -> bool {
    let m = keystroke.modifiers;
    m.platform || m.control || m.function
}

/// The non-text key a key press stands for (Shift and Alt are ignored).
fn named_key(keystroke: &Keystroke) -> Option<Key> {
    if is_shortcut(keystroke) {
        return None;
    }
    Some(match keystroke.key.as_str() {
        "backspace" => Key::Backspace,
        "enter" => Key::Enter,
        "tab" => Key::Tab,
        "escape" => Key::Escape,
        "up" => Key::ArrowUp,
        "down" => Key::ArrowDown,
        "left" => Key::ArrowLeft,
        "right" => Key::ArrowRight,
        _ => return None,
    })
}

/// The printable text a key press types on the device; named keys go through [`named_key`].
fn key_text(keystroke: &Keystroke) -> Option<String> {
    if is_shortcut(keystroke) {
        return None;
    }
    keystroke
        .key_char
        .clone()
        .filter(|text| !text.is_empty() && !text.chars().any(char::is_control))
}

impl Focusable for DeviceView {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

/// Copies a BGRA frame (snapshotting a live surface) into a GPUI image (which keeps BGRA bytes
/// in an `RgbaImage` container).
fn to_render_image(frame: &Frame) -> Option<Arc<RenderImage>> {
    let frame = frame.snapshot()?;
    let (stride, bgra) = frame.bytes()?;
    let row = frame.width as usize * 4;
    let mut data = Vec::with_capacity(row * frame.height as usize);
    for y in 0..frame.height as usize {
        let start = y * stride;
        data.extend_from_slice(bgra.get(start..start + row)?);
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
                s.picture.clone(),
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
                let Some((picture, painted, seq, captured_at, (w, h))) = shown else {
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
                match picture {
                    Picture::Image(image) => {
                        if let Err(err) =
                            window.paint_image(target, target, Corners::default(), image, 0, false)
                        {
                            tracing::warn!(%err, "failed to paint device frame");
                            return;
                        }
                    }
                    #[cfg(target_os = "macos")]
                    Picture::Surface(buffer) => window.paint_surface(target, buffer),
                }
                painted.set(true);
                stats.borrow_mut().painted(seq, captured_at, (w, h));
            },
        )
        .size_full();

        let overlay = self.stats.borrow().overlay();
        let theme = cx.theme();
        let screen_box = div()
            .id("device-screen")
            .relative()
            .flex_1()
            .min_h_0()
            .w_full()
            .key_context(KEY_CONTEXT)
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
            });
        // Stats sit above the screen, not on it: an overlay would hide the device's status bar.
        div()
            .flex()
            .flex_col()
            .size_full()
            .when_some(overlay, |this, text| {
                this.child(
                    div()
                        .flex_shrink_0()
                        .w_full()
                        .px_1()
                        .truncate()
                        .text_xs()
                        .font_family("Menlo")
                        .text_color(theme.muted_foreground)
                        .child(text),
                )
            })
            .child(screen_box)
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
    use super::{key_text, named_key};
    use gpui_kit::{Keystroke, Modifiers};
    use hangar_devices::Key;

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
        assert_eq!(key_text(&key("enter", Some("\n"), none)), None);
        assert_eq!(key_text(&key("backspace", None, none)), None);
    }

    #[test]
    fn named_keys_map_to_input_keys() {
        let none = Modifiers::default();
        let k = |name: &str| named_key(&key(name, None, none));
        assert_eq!(k("backspace"), Some(Key::Backspace));
        assert_eq!(k("enter"), Some(Key::Enter));
        assert_eq!(k("tab"), Some(Key::Tab));
        assert_eq!(k("escape"), Some(Key::Escape));
        assert_eq!(k("up"), Some(Key::ArrowUp));
        assert_eq!(k("right"), Some(Key::ArrowRight));
        assert_eq!(k("a"), None);
        assert_eq!(
            named_key(&key("tab", None, Modifiers::shift())),
            Some(Key::Tab)
        );
        assert_eq!(named_key(&key("left", None, Modifiers::command())), None);
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
