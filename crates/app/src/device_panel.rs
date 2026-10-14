//! The device panel: the live screen of the focused workspace's device (from the device pool),
//! the active backend and the platform's hardware controls. The picker sets the workspace's
//! base device. With no workspace focused the panel opens any listed device directly.

mod connection;
mod controls;
mod logs;
mod picker;
mod platform;
mod render;

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use gpui_kit::component::h_flex;
use gpui_kit::component::select::{SelectEvent, SelectItem, SelectState};
use gpui_kit::*;
use hangar_core::WorkspaceId;
use hangar_core::device::Input;

use logs::LogsPane;

use crate::device_pool::{DevicePool, DeviceStatus};
use crate::device_view::{DeviceView, InputResult};
use crate::ui::{ActiveTokens as _, Tokens};
pub use logs::LogsSettings;
pub use picker::{DeviceEntry, DeviceKey, Effect, Picker};
pub use platform::{BackendReason, Control, Listing, Platform, PlatformUi, list_all};

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

    /// Follows the pool: attaches a view to the workspace's open device and drops it when the
    /// device closes. Never boots: the agent, ⌘R or the Start button do, on demand.
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
        let (session, reason) = match ws {
            Some(ws) => match self.pool.read(cx).status(ws) {
                DeviceStatus::Open { session, reason } => (Some(session.clone()), reason.cloned()),
                _ => (None, None),
            },
            None => (None, None),
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
            (_, None) | (None, Some(_)) => self.attached = None,
        }
        cx.notify();
    }

    /// Starts (boots if needed, then opens) the focused workspace's device.
    pub fn start_workspace_device(&mut self, ws: WorkspaceId, cx: &mut Context<Self>) {
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
}

/// `HANGAR_DEVICE_STATS=1` turns the stats overlay on regardless of the saved setting.
fn stats_forced() -> bool {
    std::env::var_os("HANGAR_DEVICE_STATS").is_some_and(|v| v == "1")
}

/// The base device's name and runtime. Its state is the item's dot; the label beside the select
/// shows the state of the device actually shown (often the workspace's own clone).
fn entry_title(e: &DeviceEntry) -> String {
    format!("{} · {}", e.info.name, e.info.runtime)
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
