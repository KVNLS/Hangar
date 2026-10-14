//! GPUI window: booted-device list, live frames, click → tap.

use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::sync::Arc;
use std::time::{Duration, Instant};

use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use hangar_devices::{Device, DeviceInfo, Frame, Input, geometry};

pub fn run(device: Arc<dyn Device>, booted: Vec<DeviceInfo>) -> anyhow::Result<()> {
    let stats_enabled = std::env::var_os("HANGAR_SIM_STATS").is_some_and(|v| v != "0");
    gpui_kit::application().run(move |cx| {
        gpui_kit::init(cx);
        cx.on_window_closed(|cx, _| cx.quit()).detach();
        let size = initial_window_size(device.info());
        let options = WindowOptions {
            window_bounds: Some(WindowBounds::Windowed(Bounds::centered(None, size, cx))),
            ..Default::default()
        };
        let opened = gpui_kit::open_window(options, cx, |_, cx| {
            cx.new(|cx| SimView::new(device, booted, stats_enabled, cx))
        });
        if let Err(e) = opened {
            eprintln!("failed to open window: {e:#}");
            cx.quit();
        }
        cx.activate(true);
    });
    Ok(())
}

const SIDEBAR_W: f32 = 240.0;

fn initial_window_size(info: &DeviceInfo) -> Size<Pixels> {
    let (w, h) = info
        .screen
        .map_or((402.0, 874.0), |s| (s.width_pt(), s.height_pt()));
    // Show the device at ~80% of its point size next to the sidebar.
    size(
        px(SIDEBAR_W + (w * 0.8) as f32),
        px((h * 0.8) as f32 + 40.0),
    )
}

struct Shown {
    image: Arc<RenderImage>,
    captured_at: Instant,
    width: u32,
    height: u32,
}

struct Stats {
    enabled: bool,
    window_start: Instant,
    received: u32,
    painted: u32,
    latency_sum: Duration,
    latency_max: Duration,
}

impl Stats {
    fn on_paint(&mut self, captured_at: Instant) {
        if !self.enabled {
            return;
        }
        let l = captured_at.elapsed();
        self.painted += 1;
        self.latency_sum += l;
        self.latency_max = self.latency_max.max(l);
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
        eprintln!(
            "[sim-stats] received {:.1} fps, painted {:.1} fps, capture→paint avg {:.1} ms max {:.1} ms",
            f64::from(self.received) / secs,
            f64::from(self.painted) / secs,
            avg,
            self.latency_max.as_secs_f64() * 1000.0
        );
        self.window_start = Instant::now();
        self.received = 0;
        self.painted = 0;
        self.latency_sum = Duration::ZERO;
        self.latency_max = Duration::ZERO;
    }
}

struct SimView {
    device: Arc<dyn Device>,
    booted: Vec<DeviceInfo>,
    shown: Option<Shown>,
    retired: Vec<Arc<RenderImage>>,
    screen_bounds: Rc<Cell<Bounds<Pixels>>>,
    status: SharedString,
    stats: Rc<RefCell<Stats>>,
    _frames: Task<()>,
}

impl SimView {
    fn new(
        device: Arc<dyn Device>,
        booted: Vec<DeviceInfo>,
        stats_enabled: bool,
        cx: &mut Context<Self>,
    ) -> Self {
        let mut rx = device.frames();
        let frames = cx.spawn(async move |this, cx| {
            loop {
                if rx.changed().await.is_err() {
                    break;
                }
                let Some(frame) = rx.borrow_and_update().clone() else {
                    continue;
                };
                // BGRA → RenderImage copy happens off the main thread.
                let built = cx
                    .background_executor()
                    .spawn(async move { to_render_image(&frame).map(|img| (img, frame)) })
                    .await;
                let Some((image, frame)) = built else {
                    continue;
                };
                let alive = this.update(cx, |view, cx| {
                    {
                        let mut st = view.stats.borrow_mut();
                        st.received += u32::from(st.enabled);
                    }
                    if let Some(old) = view.shown.take() {
                        view.retired.push(old.image);
                    }
                    view.shown = Some(Shown {
                        image,
                        captured_at: frame.captured_at,
                        width: frame.width,
                        height: frame.height,
                    });
                    cx.notify();
                });
                if alive.is_err() {
                    break;
                }
            }
        });
        let info = device.info();
        let status = format!(
            "{} · {} · backend {}",
            info.name,
            info.runtime,
            device.backend()
        );
        Self {
            device,
            booted,
            shown: None,
            retired: Vec::new(),
            screen_bounds: Rc::new(Cell::new(Bounds::default())),
            status: status.into(),
            stats: Rc::new(RefCell::new(Stats {
                enabled: stats_enabled,
                window_start: Instant::now(),
                received: 0,
                painted: 0,
                latency_sum: Duration::ZERO,
                latency_max: Duration::ZERO,
            })),
            _frames: frames,
        }
    }

    fn screen_points(&self) -> Option<(f64, f64)> {
        if let Some(s) = self.device.info().screen {
            return Some((s.width_pt(), s.height_pt()));
        }
        // Without a known scale, taps are mapped in frame pixels / 3 as a last resort.
        self.shown
            .as_ref()
            .map(|s| (f64::from(s.width) / 3.0, f64::from(s.height) / 3.0))
    }

    fn on_click(&mut self, event: &MouseDownEvent, _: &mut Window, cx: &mut Context<Self>) {
        let bounds = self.screen_bounds.get();
        let Some((w_pt, h_pt)) = self.screen_points() else {
            return;
        };
        let local = event.position - bounds.origin;
        let Some((x, y)) = geometry::view_to_device(
            f64::from(local.x),
            f64::from(local.y),
            f64::from(bounds.size.width),
            f64::from(bounds.size.height),
            w_pt,
            h_pt,
        ) else {
            return;
        };
        let device = self.device.clone();
        self.status = format!("tap ({x:.0}, {y:.0}) pt…").into();
        cx.notify();
        let task = cx
            .background_executor()
            .spawn(async move { device.send_input(Input::Tap { x, y }) });
        cx.spawn(async move |this, cx| {
            let result = task.await;
            let _ = this.update(cx, |view, cx| {
                view.status = match result {
                    Ok(()) => format!("tapped ({x:.0}, {y:.0}) pt"),
                    Err(e) => format!("tap failed: {e}"),
                }
                .into();
                cx.notify();
            });
        })
        .detach();
    }
}

fn to_render_image(frame: &Frame) -> Option<Arc<RenderImage>> {
    let row = frame.width as usize * 4;
    let mut data = Vec::with_capacity(row * frame.height as usize);
    for y in 0..frame.height as usize {
        data.extend_from_slice(frame.bgra.get(y * frame.stride..y * frame.stride + row)?);
    }
    // GPUI's RenderImage stores BGRA bytes in an `RgbaImage` container.
    let buffer = image::RgbaImage::from_raw(frame.width, frame.height, data)?;
    Some(Arc::new(RenderImage::new(vec![image::Frame::new(buffer)])))
}

impl Render for SimView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // Free atlas textures of frames that are no longer shown.
        for old in self.retired.drain(..) {
            let _ = window.drop_image(old);
        }

        let selected = self.device.info().udid.clone();
        let list = self.booted.iter().map(|d| {
            let is_sel = d.udid == selected;
            div()
                .px_2()
                .py_1()
                .rounded_md()
                .when(is_sel, |el| el.bg(rgb(0x2d3a4f)))
                .child(div().text_sm().child(format!("{} ({})", d.name, d.runtime)))
                .child(
                    div()
                        .text_xs()
                        .text_color(rgb(0x8a93a3))
                        .child(d.udid.clone()),
                )
        });

        let shown = self
            .shown
            .as_ref()
            .map(|s| (s.image.clone(), s.captured_at, s.width, s.height));
        let bounds_cell = self.screen_bounds.clone();
        let stats = self.stats.clone();
        let screen = canvas(
            move |bounds, _, _| bounds,
            move |bounds, _, window, _| {
                bounds_cell.set(bounds);
                let Some((image, captured_at, w, h)) = shown else {
                    return;
                };
                let r = geometry::contain(
                    f64::from(bounds.size.width),
                    f64::from(bounds.size.height),
                    f64::from(w),
                    f64::from(h),
                );
                let image_bounds = Bounds::new(
                    point(
                        bounds.origin.x + px(r.x as f32),
                        bounds.origin.y + px(r.y as f32),
                    ),
                    size(px(r.width as f32), px(r.height as f32)),
                );
                let _ = window.paint_image(
                    image_bounds,
                    image_bounds,
                    Corners::default(),
                    image,
                    0,
                    false,
                );
                stats.borrow_mut().on_paint(captured_at);
            },
        )
        .size_full();

        div()
            .size_full()
            .flex()
            .bg(rgb(0x14171c))
            .text_color(rgb(0xe6e9ef))
            .child(
                div()
                    .w(px(SIDEBAR_W))
                    .h_full()
                    .p_2()
                    .flex()
                    .flex_col()
                    .gap_1()
                    .border_r_1()
                    .border_color(rgb(0x2a2f38))
                    .child(
                        div()
                            .text_xs()
                            .text_color(rgb(0x8a93a3))
                            .child("BOOTED SIMULATORS"),
                    )
                    .children(list)
                    .child(div().flex_1())
                    .child(
                        div()
                            .text_xs()
                            .text_color(rgb(0x8a93a3))
                            .child(self.status.clone()),
                    ),
            )
            .child(
                div().flex_1().h_full().p_2().child(
                    div()
                        .id("screen")
                        .size_full()
                        .cursor_pointer()
                        .on_mouse_down(MouseButton::Left, cx.listener(Self::on_click))
                        .child(screen),
                ),
            )
    }
}
