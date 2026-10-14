//! GPUI window: booted-device list next to the app's `DeviceView` (live frames, click → tap,
//! drag → swipe).

use std::sync::Arc;

use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use hangar_app::device_view::{DeviceSession, DeviceView, DeviceViewEvent};
use hangar_devices::{Device, DeviceInfo};

pub fn run(device: Arc<dyn Device>, booted: Vec<DeviceInfo>) -> anyhow::Result<()> {
    let stats = std::env::var_os("HANGAR_SIM_STATS").is_some_and(|v| v != "0");
    let size = initial_window_size(device.info());
    let session = DeviceSession::start(device)?;
    gpui_kit::application().run(move |cx| {
        gpui_kit::init(cx);
        cx.on_window_closed(|cx, _| cx.quit()).detach();
        let options = WindowOptions {
            window_bounds: Some(WindowBounds::Windowed(Bounds::centered(None, size, cx))),
            ..Default::default()
        };
        let opened = gpui_kit::open_window(options, cx, |_, cx| {
            cx.new(|cx| SimView::new(session, booted, stats, cx))
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

struct SimView {
    screen: Entity<DeviceView>,
    booted: Vec<DeviceInfo>,
    status: SharedString,
    last_report: Option<String>,
    _subscriptions: Vec<Subscription>,
}

impl SimView {
    fn new(
        session: DeviceSession,
        booted: Vec<DeviceInfo>,
        stats: bool,
        cx: &mut Context<Self>,
    ) -> Self {
        let status = format!(
            "{} · {} · backend {}",
            session.info().name,
            session.info().runtime,
            session.backend()
        );
        let screen = cx.new(|cx| DeviceView::new(session, stats, cx));
        let events = cx.subscribe(&screen, |this, _, event: &DeviceViewEvent, cx| {
            let DeviceViewEvent::Input(result) = event else {
                return;
            };
            this.status = match &result.result {
                Ok(()) => format!(
                    "{:?} sent in {} ms",
                    result.input,
                    result.elapsed.as_millis()
                ),
                Err(e) => format!("{:?} failed: {e}", result.input),
            }
            .into();
            cx.notify();
        });
        // HANGAR_SIM_STATS keeps printing to stderr as in the spike.
        let reports = cx.observe(&screen, |this, screen, cx| {
            let report = screen.read(cx).stats_report();
            if report.is_some() && report != this.last_report {
                if let Some(line) = &report {
                    eprintln!("[sim-stats] {line}");
                }
                this.last_report = report;
            }
        });
        Self {
            screen,
            booted,
            status: status.into(),
            last_report: None,
            _subscriptions: vec![events, reports],
        }
    }
}

impl Render for SimView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let selected = self.screen.read(cx).info().udid.clone();
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
            .child(div().flex_1().h_full().p_2().child(self.screen.clone()))
    }
}
