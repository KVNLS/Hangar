//! The device panel: picker of available devices, the live screen of the selected one, the
//! active backend and the platform's hardware controls. One panel per window; the device is
//! shared by every workspace.

mod logs;
mod picker;
mod platform;

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use gpui_kit::component::Disableable as _;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::select::{Select, SelectEvent, SelectItem, SelectState};
use gpui_kit::component::{
    ActiveTheme as _, IconName, Selectable as _, Sizable as _, StyledExt as _, h_flex, v_flex,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use hangar_devices::Input;

use logs::{LogsEvent, LogsPane};

use crate::device_view::{DeviceSession, DeviceView, DeviceViewEvent, InputResult};
pub use logs::LogsSettings;
pub use picker::{DeviceEntry, DeviceKey, Effect, Picker};
pub use platform::{
    BackendReason, Control, Listing, Opened, Platform, list_all, open_with_fallback,
};

pub enum DevicePanelEvent {
    Close,
    /// The user picked another device (save it).
    Selected(Option<DeviceKey>),
    /// The stats overlay was toggled (save it).
    Stats(bool),
    /// The logs pane was opened/closed or its level changed (save it).
    Logs(LogsSettings),
}

#[derive(Clone, Debug, PartialEq)]
struct DeviceOption {
    value: SharedString,
    title: SharedString,
}

impl SelectItem for DeviceOption {
    type Value = SharedString;

    fn title(&self) -> SharedString {
        self.title.clone()
    }

    fn value(&self) -> &Self::Value {
        &self.value
    }
}

type DeviceSelect = SelectState<Vec<DeviceOption>>;

enum Connection {
    None,
    Opening {
        key: DeviceKey,
        name: SharedString,
        _task: Task<()>,
    },
    Open {
        key: DeviceKey,
        view: Entity<DeviceView>,
        /// Why the default backend is not in use.
        reason: Option<BackendReason>,
        logs: Entity<LogsPane>,
        _events: Subscription,
        _logs_events: Subscription,
    },
    Failed {
        name: SharedString,
        error: SharedString,
    },
}

const BOOT_TICK: Duration = Duration::from_secs(1);

/// An AVD being booted from the panel. Kept apart from [`Connection`]: the boot continues
/// when the user looks at another device.
struct Booting {
    key: DeviceKey,
    name: SharedString,
    /// Latest stage reported by the boot thread.
    stage: Arc<Mutex<String>>,
    started: Instant,
    _task: Task<()>,
    _tick: Task<()>,
}

impl Booting {
    fn status(&self) -> String {
        let stage = self.stage.lock().map(|s| s.clone()).unwrap_or_default();
        format!(
            "{}: {stage}… {} s",
            self.name,
            self.started.elapsed().as_secs()
        )
    }
}

/// Last input outcome shown under the controls.
struct InputStatus {
    text: SharedString,
    error: bool,
}

pub struct DevicePanel {
    picker: Picker,
    select: Entity<DeviceSelect>,
    connection: Connection,
    listing: Option<Task<()>>,
    input_status: Option<InputStatus>,
    booting: Option<Booting>,
    boot_error: Option<(DeviceKey, SharedString)>,
    shutting_down: bool,
    stats: bool,
    logs_settings: LogsSettings,
    /// `HANGAR_DEVICE_BACKEND` (`private`, `simctl`, `grpc` or `adb`): no automatic fallback.
    forced_backend: Option<String>,
    _subscriptions: Vec<Subscription>,
}

impl EventEmitter<DevicePanelEvent> for DevicePanel {}

impl DevicePanel {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let select = cx.new(|cx| SelectState::new(Vec::new(), None, window, cx));
        let select_sub = cx.subscribe_in(
            &select,
            window,
            |this, _, event: &SelectEvent<Vec<DeviceOption>>, window, cx| {
                let SelectEvent::Confirm(Some(value)) = event else {
                    return;
                };
                let Some(key) = this.picker.entry_by_value(value).map(|e| e.key.clone()) else {
                    return;
                };
                this.select_device(key, window, cx);
            },
        );
        let forced_backend = std::env::var("HANGAR_DEVICE_BACKEND")
            .ok()
            .filter(|v| !v.is_empty());
        Self {
            picker: Picker::default(),
            select,
            connection: Connection::None,
            listing: None,
            input_status: None,
            booting: None,
            boot_error: None,
            shutting_down: false,
            stats: stats_forced(),
            logs_settings: LogsSettings::default(),
            forced_backend,
            _subscriptions: vec![select_sub],
        }
    }

    /// Applies saved settings; call before the panel is first shown.
    pub fn restore(&mut self, selected: Option<DeviceKey>, stats: bool, cx: &mut Context<Self>) {
        self.picker.restore(selected);
        self.set_stats_inner(stats || stats_forced(), cx);
    }

    pub fn picker(&self) -> &Picker {
        &self.picker
    }

    /// The open device's view, once it is open.
    pub fn view(&self) -> Option<&Entity<DeviceView>> {
        match &self.connection {
            Connection::Open { view, .. } => Some(view),
            _ => None,
        }
    }

    /// Shown → list devices and open the selected one; hidden → close it (stops capture).
    pub fn set_visible(&mut self, visible: bool, window: &mut Window, cx: &mut Context<Self>) {
        let effect = self.picker.set_visible(visible);
        self.apply(effect, window, cx);
        if visible && self.listing.is_none() {
            self.refresh(window, cx);
        }
    }

    pub fn refresh(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.picker.listing_started();
        cx.notify();
        let task = cx.background_executor().spawn(async { list_all() });
        self.listing = Some(cx.spawn_in(window, async move |this, cx| {
            let listing = task.await;
            let _ = this.update_in(cx, |this, window, cx| this.on_listed(listing, window, cx));
        }));
    }

    fn on_listed(&mut self, listing: Listing, window: &mut Window, cx: &mut Context<Self>) {
        self.listing = None;
        for (platform, err) in &listing.errors {
            tracing::warn!(platform = platform.label(), %err, "device listing failed");
        }
        let entries: Vec<DeviceEntry> = listing
            .devices
            .into_iter()
            .map(|(platform, info)| DeviceEntry::new(platform, info))
            .collect();
        tracing::info!(devices = entries.len(), "devices listed");
        let before = self.picker.selected().cloned();
        let effect = self.picker.list_done(entries, listing.errors);
        self.sync_select(window, cx);
        if self.picker.selected() != before.as_ref() {
            cx.emit(DevicePanelEvent::Selected(self.picker.selected().cloned()));
        }
        self.apply(effect, window, cx);
        cx.notify();
    }

    fn sync_select(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let options: Vec<DeviceOption> = self
            .picker
            .entries()
            .iter()
            .map(|e| DeviceOption {
                value: e.key.value().into(),
                title: entry_title(e).into(),
            })
            .collect();
        let selected: Option<SharedString> = self.picker.selected().map(|k| k.value().into());
        self.select.update(cx, |select, cx| {
            select.set_items(options, window, cx);
            match selected {
                Some(value) => select.set_selected_value(&value, window, cx),
                None => select.set_selected_index(None, window, cx),
            }
        });
    }

    /// Selects a device (picker or dev hook).
    pub fn select_device(&mut self, key: DeviceKey, window: &mut Window, cx: &mut Context<Self>) {
        let changed = self.picker.selected() != Some(&key);
        let effect = self.picker.select(key);
        if changed {
            self.sync_select(window, cx);
            cx.emit(DevicePanelEvent::Selected(self.picker.selected().cloned()));
        }
        self.apply(effect, window, cx);
        cx.notify();
    }

    /// Boots the shut-down device `key` in the background, then lists devices and opens it.
    pub fn boot_device(&mut self, key: DeviceKey, window: &mut Window, cx: &mut Context<Self>) {
        let Some(entry) = self.picker.entry_by_value(&key.value()).cloned() else {
            return;
        };
        if self.booting.is_some() || !key.platform.can_boot(&entry.info) {
            return;
        }
        tracing::info!(device = %key.id, name = %entry.info.name, "booting device");
        self.boot_error = None;
        let name: SharedString = entry.info.name.clone().into();
        let platform = key.platform;
        let stage = Arc::new(Mutex::new("booting".to_owned()));
        let reported = stage.clone();
        let task = cx.background_executor().spawn(async move {
            platform.boot(&entry.info, &|s| {
                if let Ok(mut stage) = reported.lock() {
                    *stage = s;
                }
            })
        });
        let task_key = key.clone();
        let task = cx.spawn_in(window, async move |this, cx| {
            let result = task.await;
            let _ = this.update_in(cx, |this, window, cx| {
                this.on_booted(task_key, result, window, cx)
            });
        });
        // Redraws the stage and elapsed time; dropped with `Booting` when the boot ends.
        let tick = cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(BOOT_TICK).await;
                if this.update(cx, |_, cx| cx.notify()).is_err() {
                    break;
                }
            }
        });
        self.booting = Some(Booting {
            key,
            name,
            stage,
            started: Instant::now(),
            _task: task,
            _tick: tick,
        });
        cx.notify();
    }

    fn on_booted(
        &mut self,
        key: DeviceKey,
        result: anyhow::Result<hangar_devices::DeviceInfo>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.booting = None;
        match result {
            // The booted device keeps its key, so the listing below opens it if it is selected.
            Ok(info) => {
                tracing::info!(device = %info.udid, name = %info.name, "device booted");
            }
            Err(err) => {
                tracing::error!(device = %key.id, "failed to boot device: {err:#}");
                self.boot_error = Some((key, format!("{err:#}").into()));
            }
        }
        self.refresh(window, cx);
    }

    /// Shuts the open emulator down in the background; the panel closes it once it is off.
    pub fn shutdown_device(&mut self, cx: &mut Context<Self>) {
        let Connection::Open { key, view, .. } = &self.connection else {
            return;
        };
        if self.shutting_down || !key.platform.can_shutdown(view.read(cx).info()) {
            return;
        }
        tracing::info!(device = %key.id, "shutting device down");
        self.shutting_down = true;
        self.input_status = None;
        view.read(cx).shutdown();
        cx.notify();
    }

    fn on_shutdown(
        &mut self,
        result: &Result<(), String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.shutting_down = false;
        match result {
            Ok(()) => {
                if let Connection::Open { key, .. } = &self.connection {
                    let key = key.clone();
                    self.picker.closed(&key);
                }
                self.connection = Connection::None;
                self.input_status = None;
                self.refresh(window, cx);
            }
            Err(err) => {
                self.input_status = Some(InputStatus {
                    text: format!("Shutdown failed: {err}").into(),
                    error: true,
                });
            }
        }
        cx.notify();
    }

    fn apply(&mut self, effect: Effect, window: &mut Window, cx: &mut Context<Self>) {
        if effect.close {
            if let Connection::Open { key, .. } | Connection::Opening { key, .. } = &self.connection
            {
                tracing::info!(device = %key.id, "closing device");
            }
            // Dropping the session only closes a channel: the device is released on its
            // own input thread.
            self.connection = Connection::None;
            self.input_status = None;
            self.shutting_down = false;
        }
        if let Some(entry) = effect.open {
            self.open(entry, window, cx);
        }
        cx.notify();
    }

    fn open(&mut self, entry: DeviceEntry, window: &mut Window, cx: &mut Context<Self>) {
        let DeviceEntry { key, info, .. } = entry;
        let name: SharedString = info.name.clone().into();
        tracing::info!(device = %key.id, name = %info.name, "opening device");
        let forced = self.forced_backend.clone();
        let platform = key.platform;
        let task = cx.background_executor().spawn(async move {
            let opened = platform.open(info, forced.as_deref())?;
            let session = DeviceSession::start(Arc::from(opened.device))?;
            anyhow::Ok((session, opened.reason))
        });
        let task_key = key.clone();
        let task = cx.spawn_in(window, async move |this, cx| {
            let result = task.await;
            let _ = this.update_in(cx, |this, window, cx| {
                this.on_opened(task_key, result, window, cx)
            });
        });
        self.connection = Connection::Opening {
            key,
            name,
            _task: task,
        };
        self.input_status = None;
    }

    fn on_opened(
        &mut self,
        key: DeviceKey,
        result: anyhow::Result<(DeviceSession, Option<BackendReason>)>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let name = match &self.connection {
            Connection::Opening { key: k, name, .. } if *k == key => name.clone(),
            // Superseded (closed or switched): the stale session is dropped here.
            _ => return,
        };
        match result {
            Ok((session, reason)) => {
                tracing::info!(
                    device = %key.id,
                    backend = session.backend(),
                    reason = ?reason,
                    "device opened"
                );
                let stats = self.stats;
                let view = cx.new(|cx| DeviceView::new(session, stats, cx));
                let events = cx.subscribe_in(
                    &view,
                    window,
                    |this, _, event: &DeviceViewEvent, window, cx| {
                        match event {
                            DeviceViewEvent::Input(result) => {
                                this.input_status = Some(input_status(result));
                            }
                            DeviceViewEvent::Text(result) => {
                                this.input_status = Some(InputStatus {
                                    text: format!(
                                        "Typing {} failed: {}",
                                        plural_chars(result.chars),
                                        result.error
                                    )
                                    .into(),
                                    error: true,
                                });
                            }
                            DeviceViewEvent::Shutdown(result) => {
                                this.on_shutdown(result, window, cx);
                            }
                            DeviceViewEvent::Rotate(orientation, result) => {
                                this.input_status = Some(InputStatus {
                                    text: match result {
                                        Ok(()) => format!("Rotated to {orientation:?}"),
                                        Err(e) => format!("Rotate failed: {e}"),
                                    }
                                    .into(),
                                    error: result.is_err(),
                                });
                            }
                        }
                        cx.notify();
                    },
                );
                let (logs, logs_events) = self.new_logs(&key, &view, window, cx);
                self.connection = Connection::Open {
                    key,
                    view,
                    reason,
                    logs,
                    _events: events,
                    _logs_events: logs_events,
                };
            }
            Err(err) => {
                tracing::error!(device = %key.id, "failed to open device: {err:#}");
                self.picker.open_failed(&key);
                self.connection = Connection::Failed {
                    name,
                    error: format!("{err:#}").into(),
                };
            }
        }
        cx.notify();
    }

    /// Applies saved logs pane settings; call before the panel is first shown.
    pub fn restore_logs(&mut self, settings: LogsSettings) {
        self.logs_settings = settings;
    }

    /// Opens or closes the logs pane (now, or when the device opens).
    pub fn set_logs_open(&mut self, open: bool, cx: &mut Context<Self>) {
        self.logs_settings.open = open;
        if let Connection::Open { logs, .. } = &self.connection {
            logs.update(cx, |logs, cx| logs.set_open(open, cx));
        }
    }

    fn new_logs(
        &self,
        key: &DeviceKey,
        view: &Entity<DeviceView>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> (Entity<LogsPane>, Subscription) {
        let source = view.read(cx).log_source();
        let history = key.platform == Platform::Android;
        let settings = self.logs_settings;
        let logs = cx.new(|cx| LogsPane::new(source, history, settings, window, cx));
        let sub = cx.subscribe(&logs, |this, _, event: &LogsEvent, cx| {
            let LogsEvent::Settings(settings) = event;
            this.logs_settings = *settings;
            cx.emit(DevicePanelEvent::Logs(*settings));
        });
        (logs, sub)
    }

    pub fn stats(&self) -> bool {
        self.stats
    }

    pub fn set_stats(&mut self, stats: bool, cx: &mut Context<Self>) {
        if stats != self.stats {
            self.set_stats_inner(stats, cx);
            cx.emit(DevicePanelEvent::Stats(stats));
        }
    }

    fn set_stats_inner(&mut self, stats: bool, cx: &mut Context<Self>) {
        self.stats = stats;
        if let Some(view) = self.view().cloned() {
            view.update(cx, |view, cx| view.set_stats(stats, cx));
        }
        cx.notify();
    }

    /// Sends input to the open device. Returns `false` when no device is open.
    pub fn send(&mut self, input: Input, cx: &mut Context<Self>) -> bool {
        let Some(view) = self.view() else {
            return false;
        };
        view.read(cx).send(input);
        self.input_status = None;
        cx.notify();
        true
    }

    /// Types text into the open device. Returns `false` when no device is open.
    pub fn send_text(&mut self, text: String, cx: &mut Context<Self>) -> bool {
        let Some(view) = self.view() else {
            return false;
        };
        view.read(cx).send_text(text);
        self.input_status = None;
        cx.notify();
        true
    }

    /// The error of the last input, if it failed.
    pub fn input_error(&self) -> Option<SharedString> {
        self.input_status
            .as_ref()
            .filter(|s| s.error)
            .map(|s| s.text.clone())
    }

    fn render_header(&self, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let theme = cx.theme();
        h_flex()
            .h_8()
            .px_2()
            .gap_1()
            .flex_shrink_0()
            .items_center()
            .border_b_1()
            .border_color(theme.border)
            .text_xs()
            .child(div().font_semibold().child("Device"))
            .child(div().flex_1())
            .child(
                Button::new("device-stats")
                    .ghost()
                    .xsmall()
                    .label("Stats")
                    .selected(self.stats)
                    .tooltip("Debug overlay: fps and capture → paint latency (⌘⌥⇧P)")
                    .on_click(cx.listener(|this, _, _, cx| {
                        let stats = !this.stats;
                        this.set_stats(stats, cx);
                    })),
            )
            .child(
                Button::new("device-refresh")
                    .ghost()
                    .xsmall()
                    .icon(IconName::RefreshCw)
                    .loading(self.picker.is_listing())
                    .tooltip("Refresh the device list")
                    .on_click(cx.listener(|this, _, window, cx| this.refresh(window, cx))),
            )
            .child(
                Button::new("device-close")
                    .ghost()
                    .xsmall()
                    .icon(IconName::PanelRightClose)
                    .tooltip("Hide device (⌘⇧P)")
                    .on_click(cx.listener(|_, _, _, cx| cx.emit(DevicePanelEvent::Close))),
            )
    }

    fn render_picker(&self, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let theme = cx.theme();
        let errors = self.picker.errors().iter().map(|(platform, err)| {
            div()
                .text_xs()
                .text_color(theme.danger)
                .child(format!("{}: {err}", platform.label()))
        });
        let empty = self.picker.listed() && !self.picker.entries().iter().any(|e| e.ready);
        let hints = empty.then(|| {
            Platform::ALL.map(|p| {
                div()
                    .text_xs()
                    .text_color(theme.muted_foreground)
                    .child(format!(
                        "No {} device ready. {}",
                        p.label(),
                        p.not_ready_hint()
                    ))
            })
        });
        v_flex()
            .px_2()
            .py_1()
            .gap_1()
            .flex_shrink_0()
            .child(
                Select::new(&self.select)
                    .small()
                    .placeholder(if self.picker.listed() {
                        "Select a device"
                    } else {
                        "Listing devices…"
                    }),
            )
            .children(errors)
            .children(hints.into_iter().flatten())
    }

    fn render_backend(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let theme = cx.theme();
        let Connection::Open { view, reason, .. } = &self.connection else {
            return None;
        };
        let backend = view.read(cx).backend();
        let (label, color, why) = match reason {
            None => (format!("backend: {backend}"), theme.success, None),
            Some(BackendReason::Fallback(why)) => (
                format!("backend: {backend} (fallback)"),
                theme.warning,
                Some(why.clone()),
            ),
            Some(BackendReason::Forced(why)) => (
                format!("backend: {backend} (forced)"),
                theme.muted_foreground,
                Some(why.clone()),
            ),
        };
        let note = (backend == "simctl")
            .then_some("simctl captures ~2 fps screenshots; input may be unavailable.");
        Some(
            v_flex()
                .px_2()
                .gap_0p5()
                .flex_shrink_0()
                .text_xs()
                .child(
                    h_flex()
                        .gap_1()
                        .items_center()
                        .child(div().size_2().rounded_full().bg(color))
                        .child(div().text_color(color).child(label)),
                )
                .when_some(why, |this, why| {
                    this.child(div().text_color(theme.muted_foreground).child(why))
                })
                .when_some(note, |this, note| {
                    this.child(div().text_color(theme.muted_foreground).child(note))
                })
                .into_any_element(),
        )
    }

    fn render_boot_body(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let entry = self.picker.selected_entry()?;
        let theme = cx.theme();
        let centered = || {
            v_flex()
                .flex_1()
                .min_h_0()
                .p_4()
                .gap_2()
                .items_center()
                .justify_center()
                .text_xs()
        };
        if let Some(booting) = self.booting.as_ref().filter(|b| b.key == entry.key) {
            return Some(
                centered()
                    .text_color(theme.muted_foreground)
                    .child(booting.status())
                    .into_any_element(),
            );
        }
        if !entry.key.platform.can_boot(&entry.info) {
            return None;
        }
        let key = entry.key.clone();
        let error = self
            .boot_error
            .as_ref()
            .filter(|(k, _)| *k == key)
            .map(|(_, e)| e.clone());
        Some(
            centered()
                .child(
                    div()
                        .text_color(theme.muted_foreground)
                        .child(format!("{} is not running.", entry.info.name)),
                )
                .child(
                    Button::new("device-boot")
                        .small()
                        .primary()
                        .label("Boot")
                        .disabled(self.booting.is_some())
                        .tooltip("Start this virtual device")
                        .on_click(cx.listener(move |this, _, window, cx| {
                            this.boot_device(key.clone(), window, cx);
                        })),
                )
                .when_some(error, |this, error| {
                    this.child(
                        div()
                            .text_color(theme.danger)
                            .child(format!("Boot failed: {error}")),
                    )
                })
                .into_any_element(),
        )
    }

    fn render_body(&self, cx: &mut Context<Self>) -> AnyElement {
        if matches!(self.connection, Connection::None)
            && let Some(body) = self.render_boot_body(cx)
        {
            return body;
        }
        let theme = cx.theme();
        let message = |text: SharedString, color: Hsla| {
            div()
                .flex_1()
                .min_h_0()
                .p_4()
                .flex()
                .items_center()
                .justify_center()
                .text_xs()
                .text_color(color)
                .child(text)
                .into_any_element()
        };
        match &self.connection {
            Connection::Open { view, .. } => div()
                .flex_1()
                .min_h_0()
                .p_2()
                .child(view.clone())
                .into_any_element(),
            Connection::Opening { name, .. } => {
                message(format!("Opening {name}…").into(), theme.muted_foreground)
            }
            Connection::Failed { name, error } => message(
                format!("Could not open {name}: {error}").into(),
                theme.danger,
            ),
            Connection::None => {
                let text = match self.picker.selected() {
                    _ if !self.picker.listed() => "Listing devices…".into(),
                    Some(key) if self.picker.selected_entry().is_none() => {
                        format!("The selected device ({}) is not available.", key.id).into()
                    }
                    Some(_) => match self.picker.selected_entry() {
                        Some(e) if !e.ready => format!(
                            "{} is {}. {}",
                            e.info.name,
                            e.info.state,
                            e.key.platform.not_ready_hint()
                        )
                        .into(),
                        _ => SharedString::from(""),
                    },
                    None => "Select a device.".into(),
                };
                message(text, theme.muted_foreground)
            }
        }
    }

    fn render_controls(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let Connection::Open { key, view, .. } = &self.connection else {
            return None;
        };
        let can_shutdown = key.platform.can_shutdown(view.read(cx).info());
        let can_rotate = key.platform.can_rotate();
        let rotate_view = view.clone();
        let theme = cx.theme();
        let buttons = key.platform.controls().iter().map(|control| {
            let input = control.input;
            Button::new(SharedString::from(format!("device-control-{}", control.id)))
                .xsmall()
                .ghost()
                .icon(control.icon)
                .tooltip(control.tooltip)
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.send(input, cx);
                }))
        });
        let status = self.input_status.as_ref().map(|s| {
            div()
                .min_w_0()
                .truncate()
                .text_xs()
                .text_color(if s.error {
                    theme.danger
                } else {
                    theme.muted_foreground
                })
                .child(s.text.clone())
        });
        Some(
            v_flex()
                .px_2()
                .py_1()
                .gap_1()
                .flex_shrink_0()
                .border_t_1()
                .border_color(theme.border)
                .child(
                    h_flex()
                        .flex_wrap()
                        .gap_1()
                        .items_center()
                        .children(buttons)
                        .when(can_rotate, |this| {
                            this.children([(true, "Rotate left"), (false, "Rotate right")].map(
                                |(left, label)| {
                                    let view = rotate_view.clone();
                                    Button::new(SharedString::from(format!("device-rotate-{left}")))
                                        .xsmall()
                                        .ghost()
                                        .icon(if left {
                                            gpui_kit::assets::IconName::RotateCcw
                                        } else {
                                            gpui_kit::assets::IconName::RotateCw
                                        })
                                        .tooltip(label)
                                        .on_click(move |_, _, cx| {
                                            view.update(cx, |view, _| view.rotate(left));
                                        })
                                },
                            ))
                        })
                        .when(can_shutdown, |this| {
                            this.child(
                                Button::new("device-shutdown")
                                    .xsmall()
                                    .label("Shutdown")
                                    .loading(self.shutting_down)
                                    .tooltip("Shut the device down")
                                    .on_click(
                                        cx.listener(|this, _, _, cx| this.shutdown_device(cx)),
                                    ),
                            )
                        }),
                )
                .children(status)
                .into_any_element(),
        )
    }
}

impl Render for DevicePanel {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let header = self.render_header(cx);
        let picker = self.render_picker(cx);
        let backend = self.render_backend(cx);
        let body = self.render_body(cx);
        let controls = self.render_controls(cx);
        let logs = match &self.connection {
            Connection::Open { logs, .. } => Some(logs.clone()),
            _ => None,
        };
        v_flex()
            .size_full()
            .bg(cx.theme().background)
            .child(header)
            .child(picker)
            .children(backend)
            .child(body)
            .children(logs)
            .children(controls)
    }
}

/// `HANGAR_DEVICE_STATS=1` turns the stats overlay on regardless of the saved setting.
fn stats_forced() -> bool {
    std::env::var_os("HANGAR_DEVICE_STATS").is_some_and(|v| v == "1")
}

fn entry_title(e: &DeviceEntry) -> String {
    let mut title = format!("{} · {}", e.info.name, e.info.runtime);
    if !e.ready {
        title.push_str(&format!(" · {}", e.info.state));
    }
    title
}

fn plural_chars(n: usize) -> String {
    format!("{n} character{}", if n == 1 { "" } else { "s" })
}

fn input_status(result: &InputResult) -> InputStatus {
    let what = match result.input {
        Input::Tap { x, y } => format!("Tap ({x:.0}, {y:.0})"),
        Input::TouchDown { x, y } => format!("Touch down ({x:.0}, {y:.0})"),
        Input::TouchMove { x, y } => format!("Touch move ({x:.0}, {y:.0})"),
        Input::TouchUp { x, y } => format!("Touch up ({x:.0}, {y:.0})"),
        Input::Home => "Home".to_owned(),
        Input::Back => "Back".to_owned(),
        Input::Lock => "Lock".to_owned(),
        Input::VolumeUp => "Volume up".to_owned(),
        Input::VolumeDown => "Volume down".to_owned(),
        Input::AppSwitch => "App switch".to_owned(),
        Input::Key(key) => format!("Key {key:?}"),
    };
    match &result.result {
        Ok(()) => InputStatus {
            text: format!("{what}: sent in {} ms", result.elapsed.as_millis()).into(),
            error: false,
        },
        Err(err) => InputStatus {
            text: format!("{what} failed: {err}").into(),
            error: true,
        },
    }
}
