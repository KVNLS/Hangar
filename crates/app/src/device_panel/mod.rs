//! The device panel: the live screen of the focused workspace's device (from the device pool),
//! the active backend and the platform's hardware controls. The picker sets the workspace's
//! base device. With no workspace focused the panel opens any listed device directly.

mod logs;
mod picker;
mod platform;

use std::collections::HashSet;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use gpui_kit::assets::IconName as Lucide;
use gpui_kit::component::select::{Select, SelectEvent, SelectItem, SelectState};
use gpui_kit::component::spinner::Spinner;
use gpui_kit::component::{Sizable as _, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use hangar_core::WorkspaceId;
use hangar_devices::Input;

use logs::{LogsEvent, LogsPane};

use crate::device_pool::{DevicePool, DeviceStatus};
use crate::device_view::{DeviceSession, DeviceView, DeviceViewEvent, InputResult};
use crate::ui::components::{EmptyState, UiButton};
use crate::ui::{ActiveTokens as _, Tokens, TypeScale as _};
pub use logs::LogsSettings;
pub use picker::{DeviceEntry, DeviceKey, Effect, Picker};
pub use platform::{
    BackendReason, Control, Listing, Opened, Platform, PlatformDevices, PlatformUi, list_all,
    open_with_fallback,
};

/// Key context of the panel: its bindings apply while focus is inside it.
pub const KEY_CONTEXT: &str = "DevicePanel";

actions!(device_panel, [Screenshot, PressHome]);

pub enum DevicePanelEvent {
    /// Screenshot of the workspace's device, for its agent.
    Screenshot(WorkspaceId),
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
    /// Booted (green dot) or shut down (grey dot).
    ready: bool,
}

impl SelectItem for DeviceOption {
    type Value = SharedString;

    fn title(&self) -> SharedString {
        self.title.clone()
    }

    fn value(&self) -> &Self::Value {
        &self.value
    }

    fn render(&self, _: &mut Window, cx: &mut App) -> impl IntoElement {
        let t = cx.tokens();
        h_flex()
            .gap(px(6.))
            .items_center()
            .child(
                div()
                    .flex_shrink_0()
                    .size(px(6.))
                    .rounded_full()
                    .bg(if self.ready { t.grn } else { t.tx3 }),
            )
            .child(self.title.clone())
    }
}

type DeviceSelect = SelectState<Vec<DeviceOption>>;

/// The focused workspace's device, open in the pool and shown here.
struct Attached {
    ws: WorkspaceId,
    view: Entity<DeviceView>,
    reason: Option<BackendReason>,
    logs: Entity<LogsPane>,
    _events: Subscription,
    _logs_events: Subscription,
}

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

/// The shown device's state as the header labels it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum DeviceState {
    Booting,
    /// Booted; the panel is connecting to it.
    Opening,
    Running,
    ShuttingDown,
    Off,
    /// The boot or open failed.
    Failed,
    /// Another workspace holds the device's lease.
    InUse,
}

impl DeviceState {
    fn label(self) -> &'static str {
        match self {
            Self::Booting => "Booting",
            Self::Opening => "Opening",
            Self::Running => "Running",
            Self::ShuttingDown => "Shutting down",
            Self::Off => "Shut down",
            Self::Failed => "Failed",
            Self::InUse => "In use",
        }
    }

    fn color(self, t: &Tokens) -> Hsla {
        match self {
            Self::Booting | Self::Opening => t.amb,
            Self::Running => t.grn,
            Self::ShuttingDown | Self::Off => t.tx3,
            Self::Failed => t.red,
            Self::InUse => t.vio,
        }
    }

    /// A workspace device as the pool reports it; `held` when another workspace holds it.
    fn of_workspace(status: &DeviceStatus<'_>, held: bool) -> Option<Self> {
        Some(match status {
            DeviceStatus::Unknown => return None,
            DeviceStatus::Opening { .. } | DeviceStatus::Open { .. } if held => Self::InUse,
            DeviceStatus::Closed {
                error, conflict, ..
            } if error.is_some() || conflict.is_some() => Self::Failed,
            DeviceStatus::Closed { .. } => Self::Off,
            DeviceStatus::Opening { .. } => Self::Booting,
            DeviceStatus::Open { .. } => Self::Running,
            DeviceStatus::ShuttingDown { .. } => Self::ShuttingDown,
        })
    }
}

pub struct DevicePanel {
    pool: Entity<DevicePool>,
    /// The focused workspace: its pool device is shown instead of the picker's.
    workspace: Option<WorkspaceId>,
    attached: Option<Attached>,
    /// Workspaces whose device the panel already started once (not again after a shutdown).
    started: HashSet<WorkspaceId>,
    /// The workspace's base device as last shown in the select.
    synced_base: Option<DeviceKey>,
    visible: bool,
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
    _tick: Task<()>,
    _subscriptions: Vec<Subscription>,
}

impl EventEmitter<DevicePanelEvent> for DevicePanel {}

impl DevicePanel {
    pub fn new(pool: Entity<DevicePool>, window: &mut Window, cx: &mut Context<Self>) -> Self {
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
                match this.workspace {
                    Some(ws) => this.pool.update(cx, |pool, cx| {
                        pool.assign(ws, key.platform, key.id, cx);
                    }),
                    None => this.select_device(key, window, cx),
                }
            },
        );
        let pool_sub = cx.observe_in(&pool, window, |this, _, window, cx| {
            this.sync_attached(window, cx);
        });
        // Redraws boot progress (stage and elapsed time) of the workspace's device.
        let tick = cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(BOOT_TICK).await;
                let alive = this.update(cx, |this, cx| {
                    let opening = this.workspace.is_some_and(|ws| {
                        matches!(this.pool.read(cx).status(ws), DeviceStatus::Opening { .. })
                    });
                    if opening && this.visible {
                        cx.notify();
                    }
                });
                if alive.is_err() {
                    break;
                }
            }
        });
        let forced_backend = std::env::var("HANGAR_DEVICE_BACKEND")
            .ok()
            .filter(|v| !v.is_empty());
        Self {
            pool,
            workspace: None,
            attached: None,
            started: HashSet::new(),
            synced_base: None,
            visible: false,
            _tick: tick,
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
            _subscriptions: vec![select_sub, pool_sub],
        }
    }

    /// Shows `ws`'s pool device (or, with `None`, the picker's device).
    pub fn set_workspace(
        &mut self,
        ws: Option<WorkspaceId>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.workspace == ws {
            return;
        }
        self.workspace = ws;
        self.input_status = None;
        let effect = self.picker.set_visible(self.visible && ws.is_none());
        self.apply(effect, window, cx);
        self.sync_select(window, cx);
        self.sync_attached(window, cx);
    }

    pub fn workspace(&self) -> Option<WorkspaceId> {
        self.workspace
    }

    /// Follows the pool: attaches a view to the workspace's open device, drops it when the
    /// device closes, and starts the device the first time the panel shows it.
    fn sync_attached(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let base = self.workspace.and_then(|ws| {
            self.pool.read(cx).assignment(ws).map(|a| DeviceKey {
                platform: a.platform,
                id: a.base.clone(),
            })
        });
        if base != self.synced_base {
            self.synced_base = base;
            self.sync_select(window, cx);
        }
        let ws = self.workspace.filter(|_| self.visible);
        let (session, reason, closed) = match ws {
            Some(ws) => match self.pool.read(cx).status(ws) {
                DeviceStatus::Open { session, reason } => {
                    (Some(session.clone()), reason.cloned(), false)
                }
                DeviceStatus::Closed { error: None, .. } => (None, None, true),
                _ => (None, None, false),
            },
            None => (None, None, false),
        };
        match (ws, session) {
            (Some(ws), Some(session)) => {
                let same = self
                    .attached
                    .as_ref()
                    .is_some_and(|a| a.ws == ws && a.view.read(cx).session().same(&session));
                if !same {
                    tracing::info!(workspace = %ws, device = %session.info().udid, "panel attached to the workspace device");
                    let stats = self.stats;
                    let view = cx.new(|cx| DeviceView::new(session, stats, cx));
                    let events = self.subscribe_view(&view, window, cx);
                    let platform = self
                        .pool
                        .read(cx)
                        .assignment(ws)
                        .map_or(Platform::Ios, |a| a.platform);
                    let (logs, logs_events) = self.new_logs(platform, &view, window, cx);
                    self.attached = Some(Attached {
                        ws,
                        view,
                        reason,
                        logs,
                        _events: events,
                        _logs_events: logs_events,
                    });
                    self.shutting_down = false;
                }
            }
            (ws, None) => {
                self.attached = None;
                if let Some(ws) = ws
                    && closed
                    && self.started.insert(ws)
                {
                    self.start_workspace_device(ws, cx);
                }
            }
            (None, Some(_)) => self.attached = None,
        }
        cx.notify();
    }

    /// Starts (boots if needed, then opens) the focused workspace's device.
    pub fn start_workspace_device(&mut self, ws: WorkspaceId, cx: &mut Context<Self>) {
        self.started.insert(ws);
        let task = self.pool.update(cx, |pool, cx| pool.ensure(ws, cx));
        cx.spawn(async move |_, _| {
            if let Err(err) = task.await {
                tracing::warn!(workspace = %ws, "workspace device not started: {err:#}");
            }
        })
        .detach();
    }

    /// Applies saved settings; call before the panel is first shown.
    pub fn restore(&mut self, selected: Option<DeviceKey>, stats: bool, cx: &mut Context<Self>) {
        self.picker.restore(selected);
        self.set_stats_inner(stats || stats_forced(), cx);
    }

    pub fn picker(&self) -> &Picker {
        &self.picker
    }

    /// The shown device's view, once it is open.
    pub fn view(&self) -> Option<&Entity<DeviceView>> {
        if self.workspace.is_some() {
            return self.attached.as_ref().map(|a| &a.view);
        }
        match &self.connection {
            Connection::Open { view, .. } => Some(view),
            _ => None,
        }
    }

    fn logs(&self) -> Option<&Entity<LogsPane>> {
        if self.workspace.is_some() {
            return self.attached.as_ref().map(|a| &a.logs);
        }
        match &self.connection {
            Connection::Open { logs, .. } => Some(logs),
            _ => None,
        }
    }

    /// Shown → list devices and open the shown one; hidden → close the panel's views (pool
    /// devices stay open in the pool; a picker device stops capturing).
    pub fn set_visible(&mut self, visible: bool, window: &mut Window, cx: &mut Context<Self>) {
        self.visible = visible;
        let effect = self.picker.set_visible(visible && self.workspace.is_none());
        self.apply(effect, window, cx);
        self.sync_attached(window, cx);
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
        let mut seen = HashSet::new();
        let options: Vec<DeviceOption> = self
            .picker
            .entries()
            .iter()
            // The pool's own devices are not bases; several instances of one AVD are one choice.
            .filter(|e| {
                !e.info
                    .name
                    .starts_with(crate::device_pool::backend::CLONE_PREFIX)
            })
            .filter(|e| seen.insert(e.key.value()))
            .map(|e| DeviceOption {
                value: e.key.value().into(),
                title: entry_title(e).into(),
                ready: e.ready,
            })
            .collect();
        let selected: Option<SharedString> = match self.workspace {
            Some(ws) => self.pool.read(cx).assignment(ws).map(|a| {
                DeviceKey {
                    platform: a.platform,
                    id: a.base.clone(),
                }
                .value()
                .into()
            }),
            None => self.picker.selected().map(|k| k.value().into()),
        };
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
        if let Some(ws) = self.workspace {
            if self.shutting_down {
                return;
            }
            let Some(key) = self.pool.read(cx).key_for(ws) else {
                return;
            };
            self.shutting_down = true;
            self.input_status = None;
            let task = self.pool.update(cx, |pool, cx| pool.shutdown(&key, cx));
            cx.spawn(async move |this, cx| {
                let result = task.await;
                let _ = this.update(cx, |this, cx| {
                    this.shutting_down = false;
                    if let Err(err) = result {
                        this.input_status = Some(InputStatus {
                            text: format!("Shutdown failed: {err:#}").into(),
                            error: true,
                        });
                    }
                    cx.notify();
                });
            })
            .detach();
            cx.notify();
            return;
        }
        let Connection::Open { key, view, .. } = &self.connection else {
            return;
        };
        if self.shutting_down || !view.read(cx).session().capabilities().shutdown {
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
                let events = self.subscribe_view(&view, window, cx);
                let (logs, logs_events) = self.new_logs(key.platform, &view, window, cx);
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

    fn subscribe_view(
        &mut self,
        view: &Entity<DeviceView>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Subscription {
        cx.subscribe_in(
            view,
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
                    // The pool owns workspace devices and reports their shutdown itself.
                    DeviceViewEvent::Shutdown(result) if this.workspace.is_some() => {
                        this.shutting_down = false;
                        if let Err(err) = result {
                            this.input_status = Some(InputStatus {
                                text: format!("Shutdown failed: {err}").into(),
                                error: true,
                            });
                        }
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
        )
    }

    /// Applies saved logs pane settings; call before the panel is first shown.
    pub fn restore_logs(&mut self, settings: LogsSettings) {
        self.logs_settings = settings;
    }

    /// Opens or closes the logs pane (now, or when the device opens).
    pub fn set_logs_open(&mut self, open: bool, cx: &mut Context<Self>) {
        self.logs_settings.open = open;
        if let Some(logs) = self.logs().cloned() {
            logs.update(cx, |logs, cx| logs.set_open(open, cx));
        }
    }

    fn new_logs(
        &self,
        platform: Platform,
        view: &Entity<DeviceView>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> (Entity<LogsPane>, Subscription) {
        let source = view.read(cx).handle();
        let history = platform == Platform::Android;
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

    /// A workspace device is on screen: its screenshot can go to the workspace's agent.
    fn can_screenshot(&self) -> bool {
        self.workspace.is_some() && self.attached.is_some()
    }

    /// Asks the shell to save a screenshot of the workspace's device for its agent.
    pub fn screenshot(&mut self, cx: &mut Context<Self>) {
        if let Some(ws) = self.workspace.filter(|_| self.can_screenshot()) {
            cx.emit(DevicePanelEvent::Screenshot(ws));
        }
    }

    /// The Home button (⌘⇧H), when the shown device takes it.
    fn press_home(&mut self, cx: &mut Context<Self>) {
        let accepts = self
            .view()
            .is_some_and(|v| v.read(cx).session().capabilities().accepts(&Input::Home));
        if accepts {
            self.send(Input::Home, cx);
        }
    }

    /// The error of the last input, if it failed.
    pub fn input_error(&self) -> Option<SharedString> {
        self.input_status
            .as_ref()
            .filter(|s| s.error)
            .map(|s| s.text.clone())
    }

    /// The shown device's state for the header, when there is one to tell.
    fn device_state(&self, cx: &App) -> Option<DeviceState> {
        if let Some(ws) = self.workspace {
            let pool = self.pool.read(cx);
            let held = pool.held_by_other(ws).is_some();
            return DeviceState::of_workspace(&pool.status(ws), held);
        }
        let entry = self.picker.selected_entry();
        let key = entry.map(|e| &e.key);
        if self.booting.as_ref().is_some_and(|b| Some(&b.key) == key) {
            return Some(DeviceState::Booting);
        }
        match &self.connection {
            Connection::Open { .. } if self.shutting_down => Some(DeviceState::ShuttingDown),
            Connection::Open { .. } => Some(DeviceState::Running),
            Connection::Opening { .. } => Some(DeviceState::Opening),
            Connection::Failed { .. } => Some(DeviceState::Failed),
            Connection::None => {
                let entry = entry?;
                Some(
                    if self
                        .boot_error
                        .as_ref()
                        .is_some_and(|(k, _)| *k == entry.key)
                    {
                        DeviceState::Failed
                    } else if entry.ready {
                        DeviceState::Running
                    } else {
                        DeviceState::Off
                    },
                )
            }
        }
    }

    /// 40px toolbar: device select and its state, stats overlay toggle, refresh.
    fn render_header(&self, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let t = cx.tokens();
        let state = self.device_state(cx).map(|state| {
            let color = state.color(t);
            h_flex()
                .flex_shrink_0()
                .gap(px(5.))
                .items_center()
                .font_mono()
                .text_size(px(11.))
                .text_color(color)
                .whitespace_nowrap()
                .child(div().size(px(6.)).rounded_full().bg(color))
                .child(state.label())
        });
        h_flex()
            .h(px(40.))
            .px_2()
            .gap_1()
            .flex_shrink_0()
            .items_center()
            .border_b_1()
            .border_color(t.line)
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .child(Select::new(&self.select).small().placeholder(
                        if self.picker.listed() {
                            "Select a device"
                        } else {
                            "Listing devices…"
                        },
                    )),
            )
            .children(state)
            .child(
                UiButton::new("device-screenshot")
                    .icon(Lucide::Camera)
                    .ghost()
                    .disabled(!self.can_screenshot())
                    .tooltip("Screenshot ⌘S")
                    .on_click(cx.listener(|this, _, _, cx| this.screenshot(cx))),
            )
            .child(
                UiButton::new("device-stats")
                    .icon(Lucide::Activity)
                    .ghost()
                    .selected(self.stats)
                    .tooltip("Debug overlay: fps and capture → paint latency ⌘⌥⇧P")
                    .on_click(cx.listener(|this, _, _, cx| {
                        let stats = !this.stats;
                        this.set_stats(stats, cx);
                    })),
            )
            .child(
                UiButton::new("device-refresh")
                    .icon(Lucide::RefreshCw)
                    .ghost()
                    .disabled(self.picker.is_listing())
                    .tooltip("Refresh the device list")
                    .on_click(cx.listener(|this, _, window, cx| this.refresh(window, cx))),
            )
    }

    /// Listing errors and "nothing ready" hints under the toolbar.
    fn render_picker(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let t = cx.tokens();
        let errors: Vec<Div> = self
            .picker
            .errors()
            .iter()
            .map(|(platform, err)| {
                div()
                    .text_size(px(11.))
                    .text_color(t.red)
                    .child(format!("{}: {err}", platform.label()))
            })
            .collect();
        let empty = self.picker.listed() && !self.picker.entries().iter().any(|e| e.ready);
        let hints: Vec<Div> = if empty {
            Platform::ALL
                .map(|p| {
                    div().text_size(px(11.)).text_color(t.tx3).child(format!(
                        "No {} device ready. {}",
                        p.label(),
                        p.not_ready_hint()
                    ))
                })
                .into_iter()
                .collect()
        } else {
            Vec::new()
        };
        (!errors.is_empty() || !hints.is_empty()).then(|| {
            v_flex()
                .px_3()
                .py_1p5()
                .gap_1()
                .flex_shrink_0()
                .border_b_1()
                .border_color(t.line)
                .children(errors)
                .children(hints)
                .into_any_element()
        })
    }

    /// The device on screen: platform, view and why its backend is not the default.
    fn shown(&self, cx: &App) -> Option<(Platform, &Entity<DeviceView>, Option<&BackendReason>)> {
        if let Some(ws) = self.workspace {
            let a = self.attached.as_ref()?;
            let platform = self
                .pool
                .read(cx)
                .assignment(ws)
                .map_or(Platform::Ios, |a| a.platform);
            return Some((platform, &a.view, a.reason.as_ref()));
        }
        match &self.connection {
            Connection::Open {
                key, view, reason, ..
            } => Some((key.platform, view, reason.as_ref())),
            _ => None,
        }
    }

    fn render_backend(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let (_, view, reason) = self.shown(cx)?;
        let backend = view.read(cx).backend();
        // A workspace device is not the base shown in the select: name it.
        let backend = match self.workspace {
            Some(_) => format!("{} · {backend}", view.read(cx).info().name),
            None => backend.to_owned(),
        };
        let t = cx.tokens();
        let (label, color, why) = match reason {
            None => (format!("backend: {backend}"), t.grn, None),
            Some(BackendReason::Fallback(why)) => (
                format!("backend: {backend} (fallback)"),
                t.amb,
                Some(why.clone()),
            ),
            Some(BackendReason::Forced(why)) => (
                format!("backend: {backend} (forced)"),
                t.tx3,
                Some(why.clone()),
            ),
        };
        let note = (view.read(cx).backend() == "simctl")
            .then_some("simctl captures ~2 fps screenshots; input may be unavailable.");
        Some(
            v_flex()
                .px_3()
                .py_1()
                .gap_0p5()
                .flex_shrink_0()
                .font_mono()
                .text_size(px(11.))
                .child(
                    h_flex()
                        .gap(px(5.))
                        .items_center()
                        .child(div().size(px(6.)).rounded_full().bg(color))
                        .child(div().text_color(color).child(label)),
                )
                .when_some(why, |this, why| {
                    this.child(div().text_color(t.tx3).child(why))
                })
                .when_some(note, |this, note| {
                    this.child(div().text_color(t.tx3).child(note))
                })
                .into_any_element(),
        )
    }

    fn render_boot_body(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let entry = self.picker.selected_entry()?;
        let t = cx.tokens();
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
                    .gap_3()
                    .text_size(px(11.))
                    .text_color(t.tx2)
                    .child(Spinner::new().color(t.tx3).with_size(px(18.)))
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
                    EmptyState::new(
                        Lucide::Smartphone,
                        format!("{} is not running", entry.info.name),
                    )
                    .hint("Boot it to build and run this branch on it.")
                    .action(
                        UiButton::new("device-boot")
                            .label(format!("Boot {}", entry.info.name))
                            .primary()
                            .large()
                            .disabled(self.booting.is_some())
                            .on_click(cx.listener(move |this, _, window, cx| {
                                this.boot_device(key.clone(), window, cx);
                            })),
                    ),
                )
                .when_some(error, |this, error| {
                    this.child(
                        div()
                            .text_size(px(11.))
                            .text_color(t.red)
                            .child(format!("Boot failed: {error}")),
                    )
                })
                .into_any_element(),
        )
    }

    /// The focused workspace's device: its screen, boot progress, or what keeps it from running.
    fn render_workspace_body(&self, ws: WorkspaceId, cx: &mut Context<Self>) -> AnyElement {
        let t = cx.tokens();
        let centered = || {
            v_flex()
                .flex_1()
                .min_h_0()
                .p_4()
                .gap_2()
                .items_center()
                .justify_center()
                .text_size(px(11.))
                .text_color(t.tx2)
        };
        if let Some(a) = &self.attached {
            return div()
                .flex_1()
                .min_h_0()
                .p_2()
                .child(a.view.clone())
                .into_any_element();
        }
        let pool = self.pool.read(cx);
        let waiting = pool.is_waiting(ws);
        let single = pool
            .assignment(ws)
            .is_some_and(|a| pool.single_device(a.platform));
        match pool.status(ws) {
            DeviceStatus::Unknown => centered().child("Finding this workspace's device…"),
            DeviceStatus::Opening { name, status } => centered()
                .gap_3()
                .child(Spinner::new().color(t.tx3).with_size(px(18.)))
                .child(format!("{name}: {status}"))
                .when(waiting, |this| {
                    this.child(
                        div()
                            .text_color(t.amb)
                            .child("Waiting for the device: every device slot is in use."),
                    )
                }),
            DeviceStatus::ShuttingDown { name } => centered()
                .gap_3()
                .child(Spinner::new().color(t.tx3).with_size(px(18.)))
                .child(format!("{name}: shutting down…")),
            // Open but the view is not attached yet (attached on the next pool notification).
            DeviceStatus::Open { .. } => centered().child("Opening…"),
            DeviceStatus::Closed {
                name,
                error,
                conflict,
            } => {
                let name = name.to_owned();
                let error = error.map(str::to_owned);
                let conflict = conflict.map(|serial| {
                    UiButton::new("device-clear-conflict")
                        .label(format!("Shut down {serial} and retry"))
                        .on_click(cx.listener(move |this, _, _, cx| {
                            let task = this.pool.update(cx, |pool, cx| pool.clear_conflict(ws, cx));
                            cx.spawn(async move |_, _| {
                                if let Err(err) = task.await {
                                    tracing::warn!(workspace = %ws, "conflict not cleared: {err:#}");
                                }
                            })
                            .detach();
                        }))
                });
                centered()
                    .child(
                        EmptyState::new(Lucide::Smartphone, format!("{name} is not running"))
                            .hint(if single {
                                "Workspaces share one device on this Mac and take turns (see \
                                 Settings › Devices)."
                            } else {
                                "This workspace's own device. Start it to see the screen; ⌘R \
                                 starts it too."
                            })
                            .action(
                                UiButton::new("device-start")
                                    .label("Start device")
                                    .primary()
                                    .large()
                                    .on_click(cx.listener(move |this, _, _, cx| {
                                        this.start_workspace_device(ws, cx);
                                    })),
                            ),
                    )
                    .when_some(error, |this, error| {
                        this.child(div().max_w(px(360.)).text_color(t.red).child(error))
                    })
                    .children(conflict)
            }
        }
        .into_any_element()
    }

    fn render_body(&self, cx: &mut Context<Self>) -> AnyElement {
        if let Some(ws) = self.workspace {
            return self.render_workspace_body(ws, cx);
        }
        if matches!(self.connection, Connection::None)
            && let Some(body) = self.render_boot_body(cx)
        {
            return body;
        }
        let t = cx.tokens();
        let message = |text: SharedString, color: Hsla| {
            div()
                .flex_1()
                .min_h_0()
                .p_4()
                .flex()
                .items_center()
                .justify_center()
                .text_size(px(12.))
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
            Connection::Opening { name, .. } => message(format!("Opening {name}…").into(), t.tx2),
            Connection::Failed { name, error } => {
                message(format!("Could not open {name}: {error}").into(), t.red)
            }
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
                    None => {
                        return div()
                            .flex_1()
                            .min_h_0()
                            .flex()
                            .items_center()
                            .justify_center()
                            .child(
                                EmptyState::new(Lucide::Smartphone, "No device selected")
                                    .hint("Pick a simulator or emulator above to run this branch."),
                            )
                            .into_any_element();
                    }
                };
                message(text, t.tx2)
            }
        }
    }

    fn render_controls(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let (platform, view, _) = self.shown(cx)?;
        let capabilities = view.read(cx).session().capabilities();
        let can_shutdown = capabilities.shutdown;
        let can_rotate = capabilities.rotate;
        let rotate_view = view.clone();
        let t = cx.tokens();
        let buttons = platform
            .controls()
            .iter()
            .filter(|control| capabilities.accepts(&control.input))
            .map(|control| {
                let input = control.input;
                UiButton::new(SharedString::from(format!("device-control-{}", control.id)))
                    .ghost()
                    .small()
                    .icon(control.icon)
                    .tooltip(control.tooltip)
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.send(input, cx);
                    }))
            });
        // The row is always there so the controls above do not jump when a status appears.
        let status = div()
            .h(px(16.))
            .min_w_0()
            .truncate()
            .font_mono()
            .text_size(px(11.))
            .line_height(px(16.))
            .when_some(self.input_status.as_ref(), |row, s| {
                row.text_color(if s.error { t.red } else { t.tx3 })
                    .child(s.text.clone())
            });
        Some(
            v_flex()
                .px_2()
                .py_1()
                .gap_1()
                .flex_shrink_0()
                .border_t_1()
                .border_color(t.line)
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
                                    UiButton::new(SharedString::from(format!(
                                        "device-rotate-{left}"
                                    )))
                                    .ghost()
                                    .small()
                                    .icon(if left {
                                        gpui_kit::assets::IconName::RotateCcw
                                    } else {
                                        gpui_kit::assets::IconName::RotateCw
                                    })
                                    .tooltip(label)
                                    .on_click(
                                        move |_, _, cx| {
                                            view.update(cx, |view, _| view.rotate(left));
                                        },
                                    )
                                },
                            ))
                        })
                        .when(can_shutdown, |this| {
                            this.child(
                                UiButton::new("device-shutdown")
                                    .label(if self.shutting_down {
                                        "Shutting down…"
                                    } else {
                                        "Shut down"
                                    })
                                    .small()
                                    .disabled(self.shutting_down)
                                    .tooltip("Shut the device down")
                                    .on_click(
                                        cx.listener(|this, _, _, cx| this.shutdown_device(cx)),
                                    ),
                            )
                        }),
                )
                .child(status)
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
        let logs = self.logs().cloned();
        let t = cx.tokens();
        let waiting = self
            .workspace
            .filter(|ws| self.attached.is_some() && self.pool.read(cx).is_waiting(*ws))
            .map(|_| {
                div()
                    .px_3()
                    .py_1()
                    .flex_shrink_0()
                    .text_size(px(11.))
                    .text_color(t.amb)
                    .bg(t.ambbg)
                    .child("Waiting for the device: another workspace is using it.")
            });
        v_flex()
            .key_context(KEY_CONTEXT)
            .on_action(cx.listener(|this, _: &Screenshot, _, cx| this.screenshot(cx)))
            .on_action(cx.listener(|this, _: &PressHome, _, cx| this.press_home(cx)))
            .size_full()
            .bg(t.app)
            .child(header)
            .children(waiting)
            .children(picker)
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

#[cfg(test)]
mod tests {
    use super::{DeviceState, DeviceStatus};

    #[test]
    fn workspace_device_states() {
        let state = |status: DeviceStatus<'_>, held| DeviceState::of_workspace(&status, held);
        let closed = |error, conflict| DeviceStatus::Closed {
            name: "iPhone",
            error,
            conflict,
        };
        assert_eq!(state(DeviceStatus::Unknown, false), None);
        assert_eq!(state(closed(None, None), false), Some(DeviceState::Off));
        assert_eq!(
            state(closed(Some("boot failed"), None), false),
            Some(DeviceState::Failed)
        );
        assert_eq!(
            state(closed(None, Some("emulator-5554")), false),
            Some(DeviceState::Failed)
        );
        let opening = || DeviceStatus::Opening {
            name: "iPhone",
            status: "booting… 3 s".into(),
        };
        assert_eq!(state(opening(), false), Some(DeviceState::Booting));
        assert_eq!(state(opening(), true), Some(DeviceState::InUse));
        // A device that is off is off, whoever holds its lease.
        assert_eq!(state(closed(None, None), true), Some(DeviceState::Off));
        assert_eq!(
            state(DeviceStatus::ShuttingDown { name: "iPhone" }, false),
            Some(DeviceState::ShuttingDown)
        );
        assert_eq!(DeviceState::InUse.label(), "In use");
        assert_eq!(DeviceState::Off.label(), "Shut down");
    }
}
