//! One device per workspace. Each workspace is assigned a platform and a base device; in
//! multi-device mode it gets its own device made from the base (an iOS simulator created with
//! the base's device type and runtime, or a read-only instance of the base AVD), booted on
//! demand and kept open while the app runs, on screen or not. In single-device mode (a cap of
//! one device, detected or chosen) the workspaces of a platform share its base device.
//!
//! Every run and tool call holds a lease on the device it uses (FIFO, re-entrant per
//! workspace), which marks the device busy: at the cap, and under critical memory pressure, only
//! idle devices (no lease, not shown in the panel) are shut down, least recently used first.
//!
//! All methods run on the GPUI thread; device I/O happens on the background executor.

mod assignments;
pub mod backend;
mod boot;
mod leases;
pub mod orphans;
pub mod policy;
mod shutdown;
mod status;

use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::anyhow;
use gpui_kit::*;
use hangar_core::device::capacity::{self, DeviceCost, HostResources, Pressure};
use hangar_core::device::{Conflict, Target};
use hangar_core::ports::PortResult;
use hangar_core::ports::Store;
use hangar_core::{Setting, WorkspaceId};
use hangar_usecases::settings::SettingsExt as _;
use tokio::sync::{mpsc, oneshot};

use crate::device_panel::{BackendReason, DeviceKey, Platform};
use crate::device_view::DeviceSession;
use crate::{adapters, setting_keys};
use policy::{Assignment, LeaseQueue, Limit};

async fn booted_simulators() -> PortResult<Vec<String>> {
    let running = adapters::device_catalog(Platform::Ios).running()?;
    Ok(running.into_iter().map(|r| r.id).collect())
}

/// How often memory pressure, lease timeouts, the cap and open simulators are checked.
const MONITOR_TICK: Duration = Duration::from_secs(10);
/// How often a boot waiting for a free slot looks again.
const SLOT_POLL: Duration = Duration::from_secs(1);

/// What the pool needs to know about a workspace.
#[derive(Clone, Debug)]
pub struct WorkspaceRef {
    pub id: WorkspaceId,
    pub name: String,
    pub project: String,
    pub slug: String,
    pub root: PathBuf,
}

/// A device the pool manages: a shared base device, or one workspace's own device.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum PoolKey {
    Base(DeviceKey),
    Own(WorkspaceId),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NoticeLevel {
    Info,
    Warning,
    Error,
}

pub enum PoolEvent {
    /// Something the user should know (a refused boot, an automatic shutdown).
    Notice(NoticeLevel, String),
}

/// Why a device could not be opened. Cloned to every waiter of the same boot.
#[derive(Clone, Debug)]
enum OpenError {
    /// The runtime guard refused to boot another device (message with the numbers).
    Refused(String),
    Failed(String),
}

impl OpenError {
    fn message(&self) -> &str {
        match self {
            Self::Refused(m) | Self::Failed(m) => m,
        }
    }
}

type Opened = Result<DeviceSession, OpenError>;
type LeaseReply = oneshot::Sender<anyhow::Result<Lease>>;

/// Held while a run or tool call uses a device; dropping it releases the device.
pub struct Lease {
    key: PoolKey,
    holder: WorkspaceId,
    release: mpsc::UnboundedSender<(PoolKey, WorkspaceId)>,
}

impl Lease {
    pub fn key(&self) -> &PoolKey {
        &self.key
    }
}

impl Drop for Lease {
    fn drop(&mut self) {
        let _ = self.release.send((self.key.clone(), self.holder));
    }
}

impl std::fmt::Debug for Lease {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Lease")
            .field("key", &self.key)
            .field("holder", &self.holder)
            .finish()
    }
}

/// An open device with the lease that lets the caller use it.
pub struct Acquired {
    pub session: DeviceSession,
    pub lease: Lease,
    /// Platform of the device (also of the workspace).
    pub platform: Platform,
    /// AVD id on Android (Expo picks emulators by name).
    pub avd: Option<String>,
    /// The workspace's own device could not boot; this is another device of its platform.
    pub borrowed: bool,
}

/// What the device panel shows for a workspace.
pub enum DeviceStatus<'a> {
    /// Nothing known yet (assignment not loaded).
    Unknown,
    Closed {
        name: &'a str,
        error: Option<&'a str>,
        /// An emulator outside the pool blocks the boot ([`DevicePool::clear_conflict`]).
        conflict: Option<&'a str>,
    },
    Opening {
        name: &'a str,
        status: String,
    },
    Open {
        session: &'a DeviceSession,
        reason: Option<&'a BackendReason>,
    },
    ShuttingDown {
        name: &'a str,
    },
}

/// A row of Settings › Devices.
#[derive(Clone, Debug)]
pub struct DeviceRow {
    pub key: PoolKey,
    pub platform: Platform,
    pub name: String,
    pub state: &'static str,
    pub busy: bool,
    /// Workspaces using it.
    pub workspaces: Vec<String>,
    /// A simulator the pool created (can be deleted).
    pub clone_udid: Option<String>,
}

enum State {
    Closed,
    Opening {
        stage: Arc<Mutex<String>>,
        started: Instant,
        waiters: Vec<oneshot::Sender<Opened>>,
        _task: Task<()>,
    },
    Open {
        session: DeviceSession,
        reason: Option<BackendReason>,
    },
    ShuttingDown,
}

struct PoolDevice {
    platform: Platform,
    name: String,
    target: Target,
    state: State,
    /// Running (booted by the pool, found running, or claimed by a boot in progress).
    running: bool,
    last_used: u64,
    leases: LeaseQueue<WorkspaceId, LeaseReply>,
    error: Option<String>,
    /// The last boot was blocked by an instance running outside the pool.
    conflict: Option<Conflict>,
}

impl PoolDevice {
    fn busy(&self) -> bool {
        !self.leases.is_idle() || !matches!(self.state, State::Closed | State::Open { .. })
    }

    fn booted(&self) -> bool {
        self.running || matches!(self.state, State::Open { .. })
    }

    /// adb serial or simulator UDID of the running device, when known.
    fn device_id(&self) -> Option<String> {
        match &self.state {
            State::Open { session, .. } => Some(session.info().udid.clone()),
            _ => match &self.target {
                Target::Ios { udid } => Some(udid.clone()),
                Target::IosOwn { udid, .. } => udid.clone(),
                Target::Android { serial, .. } => serial.clone(),
            },
        }
    }
}

pub struct DevicePool {
    store: Option<Arc<dyn Store>>,
    limit: Limit,
    host: Option<HostResources>,
    host_error: Option<String>,
    pressure: Option<Pressure>,
    bases: BTreeMap<Platform, String>,
    android_cost: DeviceCost,
    assignments: HashMap<WorkspaceId, Assignment>,
    workspaces: HashMap<WorkspaceId, WorkspaceRef>,
    devices: HashMap<PoolKey, PoolDevice>,
    /// Keys whose boot waits for a free slot.
    waiting_slot: HashSet<PoolKey>,
    focused: Option<WorkspaceId>,
    tick: u64,
    single_alert_shown: bool,
    last_target: Option<DeviceKey>,
    forced_backend: Option<String>,
    release: mpsc::UnboundedSender<(PoolKey, WorkspaceId)>,
    _tasks: Vec<Task<()>>,
}

impl EventEmitter<PoolEvent> for DevicePool {}

impl DevicePool {
    pub fn new(cx: &mut Context<Self>) -> Self {
        let (release, mut released) = mpsc::unbounded_channel::<(PoolKey, WorkspaceId)>();
        let releases = cx.spawn(async move |this, cx| {
            while let Some((key, holder)) = released.recv().await {
                if this
                    .update(cx, |pool, cx| pool.release_lease(&key, holder, cx))
                    .is_err()
                {
                    break;
                }
            }
        });
        let monitor = cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(MONITOR_TICK).await;
                let pressure = cx
                    .background_executor()
                    .spawn(async { adapters::host_monitor().pressure() })
                    .await;
                let Ok(open) = this.update(cx, |pool, _| pool.open_simulators()) else {
                    break;
                };
                let booted = if open.is_empty() {
                    None
                } else {
                    Some(cx.background_executor().spawn(booted_simulators()).await)
                };
                if this
                    .update(cx, |pool, cx| {
                        pool.on_monitor(pressure, cx);
                        match booted {
                            Some(Ok(booted)) => pool.close_gone_simulators(&booted, cx),
                            Some(Err(err)) => tracing::debug!(target: "device_pool", "cannot list booted simulators: {err}"),
                            None => {}
                        }
                    })
                    .is_err()
                {
                    break;
                }
            }
        });
        Self {
            store: None,
            limit: Limit::Auto,
            host: None,
            host_error: None,
            pressure: None,
            bases: BTreeMap::new(),
            android_cost: adapters::device_catalog(Platform::Android).cost(None),
            assignments: HashMap::new(),
            workspaces: HashMap::new(),
            devices: HashMap::new(),
            waiting_slot: HashSet::new(),
            focused: None,
            tick: 0,
            single_alert_shown: false,
            last_target: None,
            forced_backend: std::env::var("HANGAR_DEVICE_BACKEND")
                .ok()
                .filter(|v| !v.is_empty()),
            release,
            _tasks: vec![releases, monitor],
        }
    }

    /// Reads the saved settings and assignments of `workspaces`, detects the host and alerts
    /// once when it can only run one device. `limit_override` (dev hook) replaces the saved
    /// limit for this run without saving it.
    pub fn load(
        &mut self,
        store: Arc<dyn Store>,
        workspaces: Vec<WorkspaceRef>,
        limit_override: Option<Limit>,
        cx: &mut Context<Self>,
    ) {
        self.store = Some(store.clone());
        for ws in &workspaces {
            self.workspaces.insert(ws.id, ws.clone());
        }
        let ids: Vec<WorkspaceId> = workspaces.iter().map(|w| w.id).collect();
        cx.spawn(async move |this, cx| {
            let loaded = cx
                .background_executor()
                .spawn(async move { load_settings(&store, &ids) })
                .await;
            let _ = this.update(cx, |pool, cx| {
                match loaded {
                    Ok(saved) => {
                        pool.limit = limit_override.or(saved.limit).unwrap_or_default();
                        pool.bases = saved.bases;
                        pool.single_alert_shown = saved.alert_shown;
                        pool.last_target = saved.last_target;
                        pool.assignments.extend(saved.assignments);
                    }
                    Err(err) => {
                        tracing::error!(target: "device_pool", "failed to read the device settings: {err:#}");
                        pool.limit = limit_override.unwrap_or_default();
                    }
                }
                if limit_override.is_some() {
                    tracing::info!(target: "device_pool", limit = ?pool.limit, "limit forced by HANGAR_DEV_DEVICE_LIMIT");
                }
                pool.detect_host(cx);
                pool.reconcile_running(cx);
                cx.notify();
            });
        })
        .detach();
    }

    fn detect_host(&mut self, cx: &mut Context<Self>) {
        let android_base = self.bases.get(&Platform::Android).cloned();
        cx.spawn(async move |this, cx| {
            let (host, cost) = cx
                .background_executor()
                .spawn(async move {
                    (
                        adapters::host_monitor().detect(),
                        adapters::device_catalog(Platform::Android).cost(android_base.as_deref()),
                    )
                })
                .await;
            let _ = this.update(cx, |pool, cx| {
                pool.android_cost = cost;
                match host {
                    Ok(host) => {
                        pool.pressure = Some(host.pressure);
                        pool.host = Some(host);
                        tracing::info!(
                            target: "device_pool",
                            host = %policy::host_summary(&host),
                            available_mb = host.available_ram_mb,
                            pressure = %host.pressure,
                            ios_slots = pool.recommended(Platform::Ios),
                            android_slots = pool.recommended(Platform::Android),
                            limit = ?pool.limit,
                            "host capacity"
                        );
                    }
                    Err(err) => {
                        tracing::warn!(target: "device_pool", "cannot read host resources, one device at a time: {err}");
                        pool.host_error = Some(err.to_string());
                    }
                }
                pool.maybe_alert_single(cx);
                pool.enforce_limit(cx);
                cx.notify();
            });
        })
        .detach();
    }

    /// Notifies once (ever) that Auto found room for only one device.
    fn maybe_alert_single(&mut self, cx: &mut Context<Self>) {
        if self.single_alert_shown || self.limit != Limit::Auto {
            return;
        }
        let Some(message) = self.single_device_explanation() else {
            return;
        };
        self.single_alert_shown = true;
        cx.emit(PoolEvent::Notice(NoticeLevel::Warning, message));
        if let Some(store) = self.store.clone() {
            cx.background_executor()
                .spawn(async move {
                    if let Err(err) = store.set_setting(&setting_keys::SINGLE_DEVICE_ALERT_SHOWN, &true) {
                        tracing::warn!(target: "device_pool", %err, "failed to save the alert flag");
                    }
                })
                .detach();
        }
    }

    /// Why Auto runs one device at a time (with the numbers), when it does.
    pub fn single_device_explanation(&self) -> Option<String> {
        if self.limit != Limit::Auto {
            return None;
        }
        let single: Vec<&str> = Platform::ALL
            .into_iter()
            .filter(|p| self.recommended(*p) == 1)
            .map(Platform::label)
            .collect();
        if single.is_empty() {
            return None;
        }
        let host = match &self.host {
            Some(host) => policy::host_summary(host),
            None => format!(
                "host resources unreadable ({})",
                self.host_error.as_deref().unwrap_or("unknown")
            ),
        };
        Some(format!(
            "This Mac ({host}) can run one {} device at a time next to the agents and builds, so \
             workspaces share one device and take turns. Change it in Settings › Devices › \
             Devices at once.",
            single.join(" or ")
        ))
    }

    pub fn limit(&self) -> Limit {
        self.limit
    }

    pub fn host(&self) -> Option<&HostResources> {
        self.host.as_ref()
    }

    pub fn pressure(&self) -> Option<Pressure> {
        self.pressure
    }

    pub fn base(&self, platform: Platform) -> Option<&str> {
        self.bases.get(&platform).map(String::as_str)
    }

    fn cost(&self, platform: Platform) -> DeviceCost {
        match platform {
            Platform::Ios => DeviceCost::ios_simulator(),
            Platform::Android => self.android_cost,
        }
    }

    /// What the host can run of this platform's devices (1 when it cannot be read).
    pub fn recommended(&self, platform: Platform) -> u32 {
        self.host
            .as_ref()
            .map_or(1, |h| capacity::recommended_slots(h, &self.cost(platform)))
    }

    /// The cap on booted devices when booting one of `platform`.
    pub fn limit_for(&self, platform: Platform) -> u32 {
        self.limit.effective(self.recommended(platform))
    }

    pub fn single_device(&self, platform: Platform) -> bool {
        self.limit_for(platform) == 1
    }

    pub fn set_limit(&mut self, limit: Limit, cx: &mut Context<Self>) {
        if limit == self.limit {
            return;
        }
        tracing::info!(target: "device_pool", ?limit, "devices at once changed");
        self.limit = limit;
        self.save(setting_keys::DEVICE_LIMIT, limit, cx);
        self.enforce_limit(cx);
        cx.notify();
    }

    /// Base device used for this platform's workspaces when the user picked none.
    pub fn set_platform_base(&mut self, platform: Platform, id: String, cx: &mut Context<Self>) {
        tracing::info!(target: "device_pool", platform = platform.label(), %id, "base device set");
        self.bases.insert(platform, id.clone());
        self.save(setting_keys::device_base(platform), id, cx);
        if platform == Platform::Android {
            self.detect_host(cx);
        }
        cx.notify();
    }

    fn save<T: serde::Serialize + Send + 'static>(
        &self,
        setting: Setting<T>,
        value: T,
        cx: &mut Context<Self>,
    ) {
        let Some(store) = self.store.clone() else {
            return;
        };
        cx.background_executor()
            .spawn(async move {
                if let Err(err) = store.set_setting(&setting, &value) {
                    let key = setting.key();
                    tracing::error!(target: "device_pool", %key, %err, "failed to save");
                }
            })
            .detach();
    }
}

struct Saved {
    limit: Option<Limit>,
    bases: BTreeMap<Platform, String>,
    alert_shown: bool,
    last_target: Option<DeviceKey>,
    assignments: HashMap<WorkspaceId, Assignment>,
}

fn load_settings(store: &dyn Store, workspaces: &[WorkspaceId]) -> anyhow::Result<Saved> {
    let mut bases = BTreeMap::new();
    for platform in Platform::ALL {
        if let Some(id) = store.get_setting(&setting_keys::device_base(platform))? {
            bases.insert(platform, id);
        }
    }
    let mut assignments = HashMap::new();
    for ws in workspaces {
        match store.get_setting(&setting_keys::device_assignment(*ws)) {
            Ok(Some(a)) => {
                assignments.insert(*ws, a);
            }
            Ok(None) => {}
            Err(err) => {
                tracing::warn!(target: "device_pool", workspace = %ws, "unreadable device assignment, using the default: {err}");
            }
        }
    }
    Ok(Saved {
        limit: store.get_setting(&setting_keys::DEVICE_LIMIT)?,
        bases,
        alert_shown: store
            .get_setting(&setting_keys::SINGLE_DEVICE_ALERT_SHOWN)?
            .unwrap_or(false),
        last_target: store.get_setting(&setting_keys::LAST_DEVICE_TARGET)?,
        assignments,
    })
}

/// The saved assignment, else a default one (saved).
fn default_assignment(
    store: &dyn Store,
    ws: WorkspaceId,
    root: &std::path::Path,
    bases: &BTreeMap<Platform, String>,
) -> anyhow::Result<Assignment> {
    if let Some(a) = store.get_setting(&setting_keys::device_assignment(ws))? {
        return Ok(a);
    }
    let last = store.get_setting(&setting_keys::LAST_DEVICE_TARGET)?;
    let builds_for = backend::stack_platforms(root);
    let platform = choose_platform(last.as_ref().map(|k| k.platform), &builds_for);
    let base = match (
        bases.get(&platform),
        last.filter(|k| k.platform == platform),
    ) {
        (Some(base), _) => base.clone(),
        (None, Some(last)) => last.id,
        (None, None) => adapters::device_catalog(platform)
            .bases()?
            .into_iter()
            .next()
            .map(|(id, _)| id)
            .ok_or_else(|| {
                anyhow!(
                    "no {} device found to run this workspace on",
                    platform.label()
                )
            })?,
    };
    let a = Assignment::new(platform, base);
    store.set_setting(&setting_keys::device_assignment(ws), &a)?;
    tracing::info!(target: "device_pool", workspace = %ws, platform = platform.label(), base = %a.base, "default device assigned");
    Ok(a)
}

/// The last choice when the repository builds for it (or nothing was detected), else the first
/// platform it builds for, else iOS.
pub fn choose_platform(last: Option<Platform>, builds_for: &[Platform]) -> Platform {
    match last {
        Some(p) if builds_for.is_empty() || builds_for.contains(&p) => p,
        _ => builds_for.first().copied().unwrap_or(Platform::Ios),
    }
}

#[cfg(test)]
mod tests {
    // Not `super::*`: the glob import of gpui would shadow `#[test]`.
    use super::{Platform, choose_platform};

    #[test]
    fn platform_follows_the_last_choice_when_the_repo_builds_for_it() {
        use Platform::*;
        assert_eq!(choose_platform(Some(Android), &[Ios, Android]), Android);
        assert_eq!(choose_platform(Some(Android), &[Ios]), Ios);
        assert_eq!(choose_platform(Some(Android), &[]), Android);
        assert_eq!(choose_platform(None, &[Android]), Android);
        assert_eq!(choose_platform(None, &[]), Ios);
    }
}
