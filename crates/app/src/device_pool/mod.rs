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

pub mod backend;
pub mod policy;

use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::anyhow;
use gpui_kit::*;
use hangar_core::{Store, WorkspaceId};
use hangar_devices::capacity::{self, DeviceCost, HostResources, Pressure};
use tokio::sync::{mpsc, oneshot};

use crate::device_panel::{BackendReason, DeviceKey, Platform};
use crate::device_view::DeviceSession;
use backend::Target;
use policy::{Acquire, Assignment, Capacity, LEASE_TIMEOUT, LeaseQueue, Limit, Slot};

const LIMIT_SETTING: &str = "device_pool:limit";
const ALERT_SETTING: &str = "device_pool:single_alert_shown";
const LAST_TARGET_SETTING: &str = "device_pool:last_target";
/// How often memory pressure, lease timeouts and the cap are checked.
const MONITOR_TICK: Duration = Duration::from_secs(10);
/// How often a boot waiting for a free slot looks again.
const SLOT_POLL: Duration = Duration::from_secs(1);

fn assignment_key(id: WorkspaceId) -> String {
    format!("device:{id}")
}

fn base_key(platform: Platform) -> String {
    format!("device_pool:base:{}", platform.id())
}

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
    store: Option<Arc<Store>>,
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
                    .spawn(async { capacity::pressure_with_override() })
                    .await;
                if this
                    .update(cx, |pool, cx| pool.on_monitor(pressure, cx))
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
            android_cost: DeviceCost::android(&hangar_devices::android::AvdCost::DEFAULT),
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
        store: Arc<Store>,
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
                        HostResources::detect(),
                        backend::cost(Platform::Android, android_base.as_deref()),
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
                    if let Err(err) = store.set_setting(ALERT_SETTING, &true) {
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
        self.save(LIMIT_SETTING, limit, cx);
        self.enforce_limit(cx);
        cx.notify();
    }

    /// Base device used for this platform's workspaces when the user picked none.
    pub fn set_platform_base(&mut self, platform: Platform, id: String, cx: &mut Context<Self>) {
        tracing::info!(target: "device_pool", platform = platform.label(), %id, "base device set");
        self.bases.insert(platform, id.clone());
        self.save(&base_key(platform), id, cx);
        if platform == Platform::Android {
            self.detect_host(cx);
        }
        cx.notify();
    }

    fn save<T: serde::Serialize + Send + 'static>(
        &self,
        key: &str,
        value: T,
        cx: &mut Context<Self>,
    ) {
        let Some(store) = self.store.clone() else {
            return;
        };
        let key = key.to_owned();
        cx.background_executor()
            .spawn(async move {
                if let Err(err) = store.set_setting(&key, &value) {
                    tracing::error!(target: "device_pool", %key, %err, "failed to save");
                }
            })
            .detach();
    }

    /// Records the workspace (name, worktree) for later calls that only have its id.
    pub fn track(&mut self, ws: WorkspaceRef) {
        self.workspaces.insert(ws.id, ws);
    }

    pub fn assignment(&self, ws: WorkspaceId) -> Option<&Assignment> {
        self.assignments.get(&ws)
    }

    /// The last platform and base chosen in the New workspace sheet.
    pub fn last_target(&self) -> Option<&DeviceKey> {
        self.last_target.as_ref()
    }

    /// Assigns a platform and base device to a workspace (New workspace sheet, device panel).
    /// A different base replaces the workspace's own device: it is shut down and, when it is
    /// a simulator the pool created, deleted.
    pub fn assign(
        &mut self,
        ws: WorkspaceId,
        platform: Platform,
        base: String,
        cx: &mut Context<Self>,
    ) {
        let previous = self.assignments.get(&ws).cloned();
        if previous
            .as_ref()
            .is_some_and(|a| a.platform == platform && a.base == base)
        {
            return;
        }
        tracing::info!(target: "device_pool", workspace = %ws, platform = platform.label(), %base, "device assigned");
        let assignment = Assignment::new(platform, base.clone());
        self.assignments.insert(ws, assignment.clone());
        self.save(&assignment_key(ws), assignment, cx);
        let last = DeviceKey { platform, id: base };
        self.last_target = Some(last.clone());
        self.save(LAST_TARGET_SETTING, last, cx);
        if let Some(previous) = previous {
            self.drop_own_device(ws, previous, cx);
        }
        self.refocus(cx);
        cx.notify();
    }

    fn drop_own_device(&mut self, ws: WorkspaceId, previous: Assignment, cx: &mut Context<Self>) {
        let key = PoolKey::Own(ws);
        let shutdown = self.shutdown(&key, cx);
        self.devices.remove(&key);
        let clone = (previous.platform == Platform::Ios)
            .then_some(previous.own)
            .flatten();
        cx.spawn(async move |_, cx| {
            if let Err(err) = shutdown.await {
                tracing::warn!(target: "device_pool", "shutdown of the replaced device failed: {err:#}");
            }
            if let Some(udid) = clone {
                let deleted = cx
                    .background_executor()
                    .spawn(async move { backend::delete_clone(&udid) })
                    .await;
                if let Err(err) = deleted {
                    tracing::warn!(target: "device_pool", "could not delete the replaced simulator: {err:#}");
                }
            }
        })
        .detach();
    }

    /// The workspace's assignment, loading or creating (and saving) it on first use: the
    /// platform of the last choice when the repository builds for it, else the first platform it
    /// builds for; the platform's base device from Settings, else a running one, else the newest.
    fn resolve_assignment(
        &mut self,
        ws: WorkspaceId,
        cx: &mut Context<Self>,
    ) -> Task<anyhow::Result<Assignment>> {
        if let Some(a) = self.assignments.get(&ws) {
            return Task::ready(Ok(a.clone()));
        }
        let Some(store) = self.store.clone() else {
            return Task::ready(Err(anyhow!("the database is not open")));
        };
        let Some(root) = self.workspaces.get(&ws).map(|w| w.root.clone()) else {
            return Task::ready(Err(anyhow!("unknown workspace")));
        };
        let bases = self.bases.clone();
        cx.spawn(async move |this, cx| {
            let resolved = cx
                .background_executor()
                .spawn(async move { default_assignment(&store, ws, &root, &bases) })
                .await?;
            this.update(cx, |pool, cx| {
                // Another call may have resolved it meanwhile; keep the first.
                let a = pool
                    .assignments
                    .entry(ws)
                    .or_insert_with(|| resolved.clone())
                    .clone();
                pool.refocus(cx);
                cx.notify();
                a
            })
        })
    }

    /// The workspace's assignment, resolved (and saved) on first use.
    pub fn assignment_for(
        &mut self,
        ws: WorkspaceId,
        cx: &mut Context<Self>,
    ) -> Task<anyhow::Result<Assignment>> {
        self.resolve_assignment(ws, cx)
    }

    /// Name of the device the workspace uses, once the pool knows it.
    pub fn device_name(&self, ws: WorkspaceId) -> Option<&str> {
        self.devices
            .get(&self.key_for(ws)?)
            .map(|d| d.name.as_str())
    }

    /// Which device the workspace uses now.
    pub fn key_for(&self, ws: WorkspaceId) -> Option<PoolKey> {
        let a = self.assignments.get(&ws)?;
        Some(self.key_of(ws, a))
    }

    fn key_of(&self, ws: WorkspaceId, a: &Assignment) -> PoolKey {
        if self.single_device(a.platform) {
            PoolKey::Base(DeviceKey {
                platform: a.platform,
                id: a.base.clone(),
            })
        } else {
            PoolKey::Own(ws)
        }
    }

    fn entry(&mut self, key: &PoolKey) -> Option<&mut PoolDevice> {
        if !self.devices.contains_key(key) {
            let device = self.new_device(key)?;
            self.devices.insert(key.clone(), device);
        }
        self.devices.get_mut(key)
    }

    fn new_device(&self, key: &PoolKey) -> Option<PoolDevice> {
        let (platform, name, target) = match key {
            PoolKey::Base(k) => (
                k.platform,
                k.id.clone(),
                match k.platform {
                    Platform::Ios => Target::Ios { udid: k.id.clone() },
                    Platform::Android => Target::Android {
                        avd: k.id.clone(),
                        serial: None,
                        shared: true,
                    },
                },
            ),
            PoolKey::Own(ws) => {
                let a = self.assignments.get(ws)?;
                let w = self.workspaces.get(ws)?;
                let name = backend::clone_name(&w.project, &w.slug);
                let target = match a.platform {
                    Platform::Ios => Target::IosOwn {
                        udid: a.own.clone(),
                        base: a.base.clone(),
                        name: name.clone(),
                    },
                    Platform::Android => Target::Android {
                        avd: a.base.clone(),
                        serial: a.serial.clone(),
                        shared: false,
                    },
                };
                (a.platform, name, target)
            }
        };
        Some(PoolDevice {
            platform,
            name,
            target,
            state: State::Closed,
            running: false,
            last_used: 0,
            leases: LeaseQueue::default(),
            error: None,
        })
    }

    fn touch(&mut self, key: &PoolKey) {
        self.tick += 1;
        let tick = self.tick;
        if let Some(d) = self.devices.get_mut(key) {
            d.last_used = tick;
        }
    }

    /// The workspace on screen: its device is never shut down to make room.
    pub fn set_focused(&mut self, ws: Option<WorkspaceId>, cx: &mut Context<Self>) {
        if self.focused == ws {
            return;
        }
        self.focused = ws;
        if let Some(ws) = ws
            && !self.assignments.contains_key(&ws)
            && self.workspaces.contains_key(&ws)
        {
            self.resolve_assignment(ws, cx).detach();
        }
        self.refocus(cx);
        cx.notify();
    }

    fn refocus(&mut self, cx: &mut Context<Self>) {
        let _ = cx;
        if let Some(key) = self.focused.and_then(|ws| self.key_for(ws)) {
            self.touch(&key);
        }
    }

    fn focused_key(&self) -> Option<PoolKey> {
        self.focused.and_then(|ws| self.key_for(ws))
    }

    fn slots(&self, except: Option<&PoolKey>) -> Vec<Slot<PoolKey>> {
        let focused = self.focused_key();
        self.devices
            .iter()
            .filter(|(k, _)| Some(*k) != except)
            .map(|(k, d)| Slot {
                key: k.clone(),
                // One shutting down already makes room.
                booted: d.booted() && !matches!(d.state, State::ShuttingDown),
                busy: d.busy(),
                focused: focused.as_ref() == Some(k),
                last_used: d.last_used,
            })
            .collect()
    }

    /// Opens the workspace's device (booting it if needed) for the panel, without a lease.
    pub fn ensure(
        &mut self,
        ws: WorkspaceId,
        cx: &mut Context<Self>,
    ) -> Task<anyhow::Result<DeviceSession>> {
        let assignment = self.resolve_assignment(ws, cx);
        cx.spawn(async move |this, cx| {
            let a = assignment.await?;
            let key = this.update(cx, |pool, _| pool.key_of(ws, &a))?;
            Self::open(this, key, cx)
                .await
                .map_err(|e| anyhow!(e.message().to_owned()))
        })
    }

    /// Opens the workspace's device under a lease (waiting in line in single-device mode).
    /// When the runtime guard refuses to boot it, waits for another running device of the same
    /// platform instead.
    pub fn acquire(
        &mut self,
        ws: WorkspaceId,
        cx: &mut Context<Self>,
    ) -> Task<anyhow::Result<Acquired>> {
        let assignment = self.resolve_assignment(ws, cx);
        cx.spawn(async move |this, cx| {
            let a = assignment.await?;
            let key = this.update(cx, |pool, _| pool.key_of(ws, &a))?;
            let lease = Self::lease(this.clone(), key.clone(), ws, cx).await?;
            let avd = (a.platform == Platform::Android).then(|| a.base.clone());
            let refused = match Self::open(this.clone(), key.clone(), cx).await {
                Ok(session) => {
                    return Ok(Acquired {
                        session,
                        lease,
                        platform: a.platform,
                        avd,
                        borrowed: false,
                    });
                }
                Err(OpenError::Failed(err)) => return Err(anyhow!(err)),
                Err(OpenError::Refused(why)) => why,
            };
            drop(lease);
            let fallback = this.update(cx, |pool, _| pool.borrowable(a.platform, &key))?;
            let Some(other) = fallback else {
                return Err(anyhow!(
                    "{refused}. No other {} device is running to share.",
                    a.platform.label()
                ));
            };
            tracing::info!(target: "device_pool", workspace = %ws, ?other, "boot refused, sharing a running device");
            let lease = Self::lease(this.clone(), other.clone(), ws, cx).await?;
            let session = Self::open(this, other, cx)
                .await
                .map_err(|e| anyhow!(e.message().to_owned()))?;
            Ok(Acquired {
                session,
                lease,
                platform: a.platform,
                avd,
                borrowed: true,
            })
        })
    }

    /// A running device of `platform` other than `key`, the one with the shortest queue.
    fn borrowable(&self, platform: Platform, key: &PoolKey) -> Option<PoolKey> {
        self.devices
            .iter()
            .filter(|(k, d)| {
                *k != key && d.platform == platform && matches!(d.state, State::Open { .. })
            })
            .min_by_key(|(_, d)| d.leases.waiting_len())
            .map(|(k, _)| k.clone())
    }

    async fn lease(
        this: WeakEntity<Self>,
        key: PoolKey,
        ws: WorkspaceId,
        cx: &mut AsyncApp,
    ) -> anyhow::Result<Lease> {
        let (tx, rx) = oneshot::channel();
        let granted = this.update(cx, |pool, cx| pool.request_lease(&key, ws, tx, cx))??;
        match granted {
            Some(lease) => Ok(lease),
            None => rx.await.map_err(|_| anyhow!("the device pool stopped"))?,
        }
    }

    fn request_lease(
        &mut self,
        key: &PoolKey,
        ws: WorkspaceId,
        reply: LeaseReply,
        cx: &mut Context<Self>,
    ) -> anyhow::Result<Option<Lease>> {
        let release = self.release.clone();
        let device = self
            .entry(key)
            .ok_or_else(|| anyhow!("no device assigned to this workspace"))?;
        let name = device.name.clone();
        let outcome = device.leases.acquire(ws, Instant::now(), reply);
        cx.notify();
        match outcome {
            Acquire::Granted(_) => {
                self.touch(key);
                Ok(Some(Lease {
                    key: key.clone(),
                    holder: ws,
                    release,
                }))
            }
            Acquire::Queued { position } => {
                tracing::info!(target: "device_pool", workspace = %ws, device = %name, position, "waiting for the device");
                Ok(None)
            }
        }
    }

    fn release_lease(&mut self, key: &PoolKey, holder: WorkspaceId, cx: &mut Context<Self>) {
        let release = self.release.clone();
        let Some(device) = self.devices.get_mut(key) else {
            return;
        };
        let granted = device.leases.release(&holder);
        let next = device.leases.holder().copied();
        for reply in granted {
            if let Some(next) = next {
                let lease = Lease {
                    key: key.clone(),
                    holder: next,
                    release: release.clone(),
                };
                // A waiter that gave up returns the lease, which releases it again.
                let _ = reply.send(Ok(lease));
            }
        }
        if let Some(next) = next {
            tracing::info!(target: "device_pool", workspace = %next, ?key, "device lease granted");
        }
        self.touch(key);
        self.enforce_limit(cx);
        cx.notify();
    }

    /// Whether the workspace waits for a device (lease queue or a free slot).
    pub fn is_waiting(&self, ws: WorkspaceId) -> bool {
        self.devices.values().any(|d| d.leases.is_waiting(&ws))
            || self
                .key_for(ws)
                .is_some_and(|k| self.waiting_slot.contains(&k))
    }

    async fn open(this: WeakEntity<Self>, key: PoolKey, cx: &mut AsyncApp) -> Opened {
        let rx = this
            .update(cx, |pool, cx| pool.open_or_join(&key, cx))
            .map_err(|_| OpenError::Failed("the device pool stopped".into()))?;
        match rx {
            Ok(session) => Ok(session),
            Err(rx) => rx
                .await
                .unwrap_or_else(|_| Err(OpenError::Failed("the device pool stopped".into()))),
        }
    }

    fn open_or_join(
        &mut self,
        key: &PoolKey,
        cx: &mut Context<Self>,
    ) -> Result<DeviceSession, oneshot::Receiver<Opened>> {
        let (tx, rx) = oneshot::channel();
        let Some(device) = self.entry(key) else {
            let _ = tx.send(Err(OpenError::Failed(
                "no device assigned to this workspace".into(),
            )));
            return Err(rx);
        };
        match &mut device.state {
            State::Open { session, .. } => return Ok(session.clone()),
            State::Opening { waiters, .. } => {
                waiters.push(tx);
                return Err(rx);
            }
            State::ShuttingDown => {
                let _ = tx.send(Err(OpenError::Failed(format!(
                    "{} is shutting down; try again in a moment",
                    device.name
                ))));
                return Err(rx);
            }
            State::Closed => {}
        }
        let stage = Arc::new(Mutex::new("checking the device".to_owned()));
        let target = device.target.clone();
        let platform = device.platform;
        let forced = self.forced_backend.clone();
        let task_key = key.clone();
        let task_stage = stage.clone();
        let task = cx.spawn(async move |this, cx| {
            let result = Self::bring_up(
                this.clone(),
                &task_key,
                target,
                platform,
                forced,
                task_stage,
                cx,
            )
            .await;
            let _ = this.update(cx, |pool, cx| pool.finish_open(&task_key, result, cx));
        });
        if let Some(device) = self.devices.get_mut(key) {
            device.error = None;
            device.state = State::Opening {
                stage,
                started: Instant::now(),
                waiters: vec![tx],
                _task: task,
            };
        }
        self.touch(key);
        cx.notify();
        Err(rx)
    }

    #[allow(clippy::too_many_arguments)]
    async fn bring_up(
        this: WeakEntity<Self>,
        key: &PoolKey,
        target: Target,
        platform: Platform,
        forced: Option<String>,
        stage: Arc<Mutex<String>>,
        cx: &mut AsyncApp,
    ) -> Result<(DeviceSession, Option<BackendReason>, Option<String>), OpenError> {
        let failed = |e: anyhow::Error| OpenError::Failed(format!("{e:#}"));
        let gone = |_| OpenError::Failed("the device pool stopped".into());
        let set_stage = |text: &str| {
            if let Ok(mut s) = stage.lock() {
                *s = text.to_owned();
            }
        };
        let probe_target = target.clone();
        let probed = cx
            .background_executor()
            .spawn(async move { backend::probe(&probe_target) })
            .await
            .map_err(failed)?;
        let running = match probed {
            Some(running) => {
                this.update(cx, |pool, _| {
                    if let Some(d) = pool.devices.get_mut(key) {
                        d.running = true;
                    }
                })
                .map_err(gone)?;
                running
            }
            None => {
                Self::claim_slot(this.clone(), key, platform, &set_stage, cx).await?;
                let cost = this
                    .update(cx, |pool, _| pool.cost(platform))
                    .map_err(gone)?;
                let host = cx
                    .background_executor()
                    .spawn(async { HostResources::detect() })
                    .await;
                let guard = match &host {
                    Ok(host) => policy::guard(Some(host), &cost),
                    Err(err) => {
                        tracing::warn!(target: "device_pool", "boot not checked against host resources: {err}");
                        Ok(())
                    }
                };
                if let Err(refusal) = guard {
                    let numbers = host.as_ref().map(policy::host_summary).unwrap_or_default();
                    tracing::warn!(target: "device_pool", ?key, %refusal, %numbers, "boot refused");
                    this.update(cx, |pool, _| {
                        if let Some(d) = pool.devices.get_mut(key) {
                            d.running = false;
                        }
                    })
                    .map_err(gone)?;
                    return Err(OpenError::Refused(format!(
                        "Not booting another {} device: {refusal} ({numbers}). Close a device \
                         or an app, or lower Devices at once in Settings",
                        platform.label()
                    )));
                }
                tracing::info!(target: "device_pool", ?key, "booting");
                let boot_target = target.clone();
                let progress_stage = stage.clone();
                let booted = cx
                    .background_executor()
                    .spawn(async move {
                        backend::boot(&boot_target, &|s| {
                            if let Ok(mut stage) = progress_stage.lock() {
                                *stage = s;
                            }
                        })
                    })
                    .await;
                match booted {
                    Ok(running) => running,
                    Err(err) => {
                        let _ = this.update(cx, |pool, _| {
                            if let Some(d) = pool.devices.get_mut(key) {
                                d.running = false;
                            }
                        });
                        return Err(failed(err));
                    }
                }
            }
        };
        set_stage("opening");
        let created = running.created.clone();
        let info = running.info;
        let (session, reason) = cx
            .background_executor()
            .spawn(async move { backend::open(platform, info, forced.as_deref()) })
            .await
            .map_err(failed)?;
        Ok((session, reason, created))
    }

    /// Waits until booting one more device stays within the cap, shutting idle devices down.
    async fn claim_slot(
        this: WeakEntity<Self>,
        key: &PoolKey,
        platform: Platform,
        set_stage: &dyn Fn(&str),
        cx: &mut AsyncApp,
    ) -> Result<(), OpenError> {
        let gone = |_| OpenError::Failed("the device pool stopped".into());
        let started = Instant::now();
        loop {
            let decision = this
                .update(cx, |pool, _| {
                    let limit = pool.limit_for(platform);
                    (policy::capacity(&pool.slots(Some(key)), limit), limit)
                })
                .map_err(gone)?;
            match decision {
                (Capacity::Free, _) => {
                    this.update(cx, |pool, cx| {
                        pool.waiting_slot.remove(key);
                        if let Some(d) = pool.devices.get_mut(key) {
                            d.running = true;
                        }
                        cx.notify();
                    })
                    .map_err(gone)?;
                    return Ok(());
                }
                (Capacity::Evict(victim), limit) => {
                    set_stage("shutting down an idle device");
                    let shutdown = this
                        .update(cx, |pool, cx| {
                            let name = pool.devices.get(&victim).map(|d| d.name.clone());
                            tracing::info!(target: "device_pool", ?victim, ?name, limit, for_key = ?key, "at the cap: shutting down the least recently used idle device");
                            pool.shutdown(&victim, cx)
                        })
                        .map_err(gone)?;
                    if let Err(err) = shutdown.await {
                        tracing::warn!(target: "device_pool", "shutdown failed: {err:#}");
                        cx.background_executor().timer(SLOT_POLL).await;
                    }
                }
                (Capacity::Full, limit) => {
                    if started.elapsed() > LEASE_TIMEOUT {
                        let _ = this.update(cx, |pool, cx| {
                            pool.waiting_slot.remove(key);
                            cx.notify();
                        });
                        return Err(OpenError::Failed(format!(
                            "all {limit} device slots stayed busy for {} minutes",
                            LEASE_TIMEOUT.as_secs() / 60
                        )));
                    }
                    set_stage("waiting for a free device slot");
                    let first = this
                        .update(cx, |pool, cx| {
                            cx.notify();
                            pool.waiting_slot.insert(key.clone())
                        })
                        .map_err(gone)?;
                    if first {
                        tracing::info!(target: "device_pool", ?key, limit, "every device slot is busy: waiting");
                    }
                    cx.background_executor().timer(SLOT_POLL).await;
                }
            }
        }
    }

    fn finish_open(
        &mut self,
        key: &PoolKey,
        result: Result<(DeviceSession, Option<BackendReason>, Option<String>), OpenError>,
        cx: &mut Context<Self>,
    ) {
        self.waiting_slot.remove(key);
        let Some(device) = self.devices.get_mut(key) else {
            return;
        };
        let State::Opening { waiters, .. } = std::mem::replace(&mut device.state, State::Closed)
        else {
            return;
        };
        let opened: Opened = match result {
            Ok((session, reason, created)) => {
                tracing::info!(
                    target: "device_pool",
                    ?key,
                    device = %session.info().udid,
                    name = %session.info().name,
                    backend = session.backend(),
                    "device open"
                );
                device.running = true;
                device.name = session.info().name.clone();
                let id = session.info().udid.clone();
                match &mut device.target {
                    Target::IosOwn { udid, .. } => *udid = Some(id.clone()),
                    Target::Android { serial, .. } => *serial = Some(id.clone()),
                    Target::Ios { .. } => {}
                }
                device.state = State::Open {
                    session: session.clone(),
                    reason,
                };
                if let PoolKey::Own(ws) = key {
                    self.record_own(*ws, created, id, cx);
                }
                Ok(session)
            }
            Err(err) => {
                tracing::warn!(target: "device_pool", ?key, error = err.message(), "device not opened");
                device.error = Some(err.message().to_owned());
                Err(err)
            }
        };
        for waiter in waiters {
            let _ = waiter.send(opened.clone());
        }
        self.enforce_limit(cx);
        cx.notify();
    }

    /// Saves what the workspace's own device is now (created simulator, Android serial).
    fn record_own(
        &mut self,
        ws: WorkspaceId,
        created: Option<String>,
        id: String,
        cx: &mut Context<Self>,
    ) {
        let Some(a) = self.assignments.get_mut(&ws) else {
            return;
        };
        let before = a.clone();
        match a.platform {
            Platform::Ios => {
                if let Some(udid) = created {
                    a.own = Some(udid);
                }
            }
            Platform::Android => {
                a.own = Some(a.base.clone());
                a.serial = Some(id);
            }
        }
        if *a != before {
            let a = a.clone();
            self.save(&assignment_key(ws), a, cx);
        }
    }

    /// Shuts a pool device down (no-op when it is not running).
    pub fn shutdown(&mut self, key: &PoolKey, cx: &mut Context<Self>) -> Task<anyhow::Result<()>> {
        let Some(device) = self.devices.get_mut(key) else {
            return Task::ready(Ok(()));
        };
        let platform = device.platform;
        match std::mem::replace(&mut device.state, State::ShuttingDown) {
            State::Open { session, .. } => {
                tracing::info!(target: "device_pool", ?key, name = %device.name, "shutting down");
                let done = session.shutdown_acked();
                let key = key.clone();
                cx.notify();
                cx.spawn(async move |this, cx| {
                    let result = done
                        .await
                        .map_err(|_| anyhow!("the device stopped before it shut down"))
                        .and_then(|r| r.map_err(anyhow::Error::msg));
                    drop(session);
                    this.update(cx, |pool, cx| pool.finish_shutdown(&key, &result, cx))?;
                    result
                })
            }
            State::Closed if device.running => {
                device.state = State::ShuttingDown;
                let Some(id) = device.device_id() else {
                    device.state = State::Closed;
                    device.running = false;
                    return Task::ready(Ok(()));
                };
                tracing::info!(target: "device_pool", ?key, %id, "shutting down (not open)");
                let key = key.clone();
                cx.notify();
                cx.spawn(async move |this, cx| {
                    let result = cx
                        .background_executor()
                        .spawn(async move { backend::shutdown(platform, &id) })
                        .await;
                    this.update(cx, |pool, cx| pool.finish_shutdown(&key, &result, cx))?;
                    result
                })
            }
            other => {
                device.state = other;
                Task::ready(Ok(()))
            }
        }
    }

    fn finish_shutdown(
        &mut self,
        key: &PoolKey,
        result: &anyhow::Result<()>,
        cx: &mut Context<Self>,
    ) {
        if let Some(device) = self.devices.get_mut(key) {
            device.state = State::Closed;
            match result {
                Ok(()) => {
                    device.running = false;
                    tracing::info!(target: "device_pool", ?key, "device shut down");
                }
                Err(err) => {
                    tracing::warn!(target: "device_pool", ?key, "shutdown failed: {err:#}");
                    device.error = Some(format!("Shutdown failed: {err:#}"));
                }
            }
        }
        cx.notify();
    }

    /// Shuts down idle devices above the cap.
    /// The cap counts booted devices of every platform; with several platforms booted the
    /// tightest of their caps applies.
    fn enforce_limit(&mut self, cx: &mut Context<Self>) {
        let Some(limit) = self
            .devices
            .values()
            .filter(|d| d.booted())
            .map(|d| self.limit_for(d.platform))
            .min()
        else {
            return;
        };
        for key in policy::over_limit(&self.slots(None), limit) {
            tracing::info!(target: "device_pool", ?key, limit, "above the cap: shutting down an idle device");
            self.shutdown(&key, cx).detach();
        }
    }

    fn on_monitor(&mut self, pressure: hangar_devices::Result<Pressure>, cx: &mut Context<Self>) {
        let now = Instant::now();
        for device in self.devices.values_mut() {
            for (ws, reply) in device.leases.expire(now, LEASE_TIMEOUT) {
                tracing::warn!(target: "device_pool", workspace = %ws, device = %device.name, "gave up waiting for the device");
                let _ = reply.send(Err(anyhow!(
                    "waited {} minutes for {} without getting it",
                    LEASE_TIMEOUT.as_secs() / 60,
                    device.name
                )));
            }
        }
        match pressure {
            Ok(pressure) => {
                if self.pressure != Some(pressure) {
                    tracing::info!(target: "device_pool", %pressure, "memory pressure");
                    self.pressure = Some(pressure);
                    cx.notify();
                }
                if pressure == Pressure::Critical
                    && let Some(victim) = policy::lru_idle(&self.slots(None))
                {
                    let name = self
                        .devices
                        .get(&victim)
                        .map(|d| d.name.clone())
                        .unwrap_or_default();
                    tracing::warn!(target: "device_pool", ?victim, %name, "critical memory pressure: shutting down an idle device");
                    cx.emit(PoolEvent::Notice(
                        NoticeLevel::Warning,
                        format!(
                            "Memory pressure is critical: shut down {name}, which nothing was \
                             using. Devices in use by a run or an agent are left alone."
                        ),
                    ));
                    self.shutdown(&victim, cx).detach();
                }
            }
            Err(err) => {
                tracing::debug!(target: "device_pool", "memory pressure unavailable: {err}")
            }
        }
        self.enforce_limit(cx);
    }

    /// The workspace is archived: its waiters give up and its own device shuts down.
    pub fn archive(&mut self, ws: WorkspaceId, cx: &mut Context<Self>) -> Task<anyhow::Result<()>> {
        for device in self.devices.values_mut() {
            for reply in device.leases.cancel(&ws) {
                let _ = reply.send(Err(anyhow!("the workspace was closed")));
            }
        }
        if self.focused == Some(ws) {
            self.focused = None;
        }
        cx.notify();
        self.shutdown(&PoolKey::Own(ws), cx)
    }

    /// The workspace is deleted: as [`Self::archive`], then its simulator (if the pool created
    /// one) and its assignment are deleted.
    pub fn delete(&mut self, ws: WorkspaceId, cx: &mut Context<Self>) -> Task<anyhow::Result<()>> {
        let shutdown = self.archive(ws, cx);
        self.devices.remove(&PoolKey::Own(ws));
        self.workspaces.remove(&ws);
        let assignment = self.assignments.remove(&ws);
        let store = self.store.clone();
        cx.spawn(async move |_, cx| {
            if let Err(err) = shutdown.await {
                tracing::warn!(target: "device_pool", "shutdown before delete failed: {err:#}");
            }
            cx.background_executor()
                .spawn(async move {
                    if let Some(store) = store {
                        store.delete_setting(&assignment_key(ws))?;
                    }
                    match assignment {
                        Some(Assignment {
                            platform: Platform::Ios,
                            own: Some(udid),
                            ..
                        }) => backend::delete_clone(&udid),
                        _ => Ok(()),
                    }
                })
                .await
        })
    }

    /// Settings › Devices: deletes a simulator the pool created for a workspace. The workspace
    /// gets a new one the next time it needs a device.
    pub fn delete_clone(
        &mut self,
        key: &PoolKey,
        cx: &mut Context<Self>,
    ) -> Task<anyhow::Result<()>> {
        let PoolKey::Own(ws) = key else {
            return Task::ready(Err(anyhow!("only devices Hangar created can be deleted")));
        };
        let ws = *ws;
        let Some(udid) = self
            .assignments
            .get(&ws)
            .filter(|a| a.platform == Platform::Ios)
            .and_then(|a| a.own.clone())
        else {
            return Task::ready(Err(anyhow!("no simulator was created for this workspace")));
        };
        let shutdown = self.shutdown(key, cx);
        self.devices.remove(key);
        if let Some(a) = self.assignments.get_mut(&ws) {
            a.own = None;
            let a = a.clone();
            self.save(&assignment_key(ws), a, cx);
        }
        cx.notify();
        cx.spawn(async move |_, cx| {
            let _ = shutdown.await;
            cx.background_executor()
                .spawn(async move { backend::delete_clone(&udid) })
                .await
        })
    }

    /// What the panel shows for the workspace.
    pub fn status(&self, ws: WorkspaceId) -> DeviceStatus<'_> {
        let Some(key) = self.key_for(ws) else {
            return DeviceStatus::Unknown;
        };
        let Some(device) = self.devices.get(&key) else {
            let name = match (&key, self.assignments.get(&ws)) {
                (PoolKey::Base(_), Some(a)) => a.base.as_str(),
                _ => "its own device",
            };
            return DeviceStatus::Closed { name, error: None };
        };
        match &device.state {
            State::Closed => DeviceStatus::Closed {
                name: &device.name,
                error: device.error.as_deref(),
            },
            State::Opening { stage, started, .. } => DeviceStatus::Opening {
                name: &device.name,
                status: format!(
                    "{}… {} s",
                    stage.lock().map(|s| s.clone()).unwrap_or_default(),
                    started.elapsed().as_secs()
                ),
            },
            State::Open { session, reason } => DeviceStatus::Open {
                session,
                reason: reason.as_ref(),
            },
            State::ShuttingDown => DeviceStatus::ShuttingDown { name: &device.name },
        }
    }

    /// The open device of the workspace, if any (no boot).
    pub fn session(&self, ws: WorkspaceId) -> Option<&DeviceSession> {
        match &self.devices.get(&self.key_for(ws)?)?.state {
            State::Open { session, .. } => Some(session),
            _ => None,
        }
    }

    /// Every device the pool knows, for Settings › Devices.
    pub fn rows(&self) -> Vec<DeviceRow> {
        let mut rows: Vec<DeviceRow> = self
            .devices
            .iter()
            .map(|(key, d)| {
                let workspaces = self
                    .assignments
                    .keys()
                    .filter(|ws| self.key_for(**ws).as_ref() == Some(key))
                    .filter_map(|ws| self.workspaces.get(ws).map(|w| w.name.clone()))
                    .collect();
                let clone_udid = match (key, &d.target) {
                    (PoolKey::Own(_), Target::IosOwn { udid, .. }) => udid.clone(),
                    _ => None,
                };
                DeviceRow {
                    key: key.clone(),
                    platform: d.platform,
                    name: d.name.clone(),
                    state: match &d.state {
                        State::Open { .. } => "open",
                        State::Opening { .. } => "starting",
                        State::ShuttingDown => "shutting down",
                        State::Closed if d.running => "running",
                        State::Closed => "off",
                    },
                    busy: d.busy(),
                    workspaces,
                    clone_udid,
                }
            })
            .collect();
        // Created simulators of workspaces whose device was never opened in this run.
        for (ws, a) in &self.assignments {
            let key = PoolKey::Own(*ws);
            if a.platform == Platform::Ios
                && a.own.is_some()
                && !self.devices.contains_key(&key)
                && let Some(w) = self.workspaces.get(ws)
            {
                rows.push(DeviceRow {
                    key,
                    platform: Platform::Ios,
                    name: backend::clone_name(&w.project, &w.slug),
                    state: "not open",
                    busy: false,
                    workspaces: vec![w.name.clone()],
                    clone_udid: a.own.clone(),
                });
            }
        }
        rows.sort_by(|a, b| a.name.cmp(&b.name));
        rows
    }

    /// Counts of booted pool devices, for the status bar and Settings.
    pub fn booted_count(&self) -> usize {
        self.devices.values().filter(|d| d.booted()).count()
    }
}

struct Saved {
    limit: Option<Limit>,
    bases: BTreeMap<Platform, String>,
    alert_shown: bool,
    last_target: Option<DeviceKey>,
    assignments: HashMap<WorkspaceId, Assignment>,
}

fn load_settings(store: &Store, workspaces: &[WorkspaceId]) -> anyhow::Result<Saved> {
    let mut bases = BTreeMap::new();
    for platform in Platform::ALL {
        if let Some(id) = store.get_setting::<String>(&base_key(platform))? {
            bases.insert(platform, id);
        }
    }
    let mut assignments = HashMap::new();
    for ws in workspaces {
        match store.get_setting::<Assignment>(&assignment_key(*ws)) {
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
        limit: store.get_setting(LIMIT_SETTING)?,
        bases,
        alert_shown: store.get_setting(ALERT_SETTING)?.unwrap_or(false),
        last_target: store.get_setting(LAST_TARGET_SETTING)?,
        assignments,
    })
}

/// The saved assignment, else a default one (saved).
fn default_assignment(
    store: &Store,
    ws: WorkspaceId,
    root: &std::path::Path,
    bases: &BTreeMap<Platform, String>,
) -> anyhow::Result<Assignment> {
    if let Some(a) = store.get_setting::<Assignment>(&assignment_key(ws))? {
        return Ok(a);
    }
    let last = store.get_setting::<DeviceKey>(LAST_TARGET_SETTING)?;
    let builds_for = backend::stack_platforms(root);
    let platform = choose_platform(last.as_ref().map(|k| k.platform), &builds_for);
    let base = match (
        bases.get(&platform),
        last.filter(|k| k.platform == platform),
    ) {
        (Some(base), _) => base.clone(),
        (None, Some(last)) => last.id,
        (None, None) => backend::bases(platform)?
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
    store.set_setting(&assignment_key(ws), &a)?;
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
