//! The device pool as a sans-IO state machine. One device per workspace: each workspace is
//! assigned a platform and a base device; in multi-device mode it gets its own device made from
//! the base (an iOS simulator created with the base's device type and runtime, or a read-only
//! instance of the base AVD), booted on demand and kept open while the app runs. In
//! single-device mode (a cap of one device, detected or chosen) the workspaces of a platform
//! share its base device.
//!
//! Every run and tool call holds a lease on the device it uses (FIFO, re-entrant per
//! workspace), which marks the device busy: at the cap, and under critical memory pressure, only
//! idle devices (no lease, not shown in the panel) are shut down, least recently used first.
//! The holder keeps the device between its calls (a reservation, see [`reservations`]) so that
//! another workspace cannot slip in between a screenshot and a tap.
//!
//! [`PoolMachine`] takes calls and I/O results together with the current [`Instant`] and queues
//! [`Command`]s (boot this, open that, shut it down, answer a waiter, save a setting); the
//! caller drains them with [`PoolMachine::take_commands`], runs them through the device ports
//! and feeds the results back. It never blocks and holds no channel: waiters are [`Token`]s.

mod assignments;
mod boot;
mod host;
mod leases;
pub mod policy;
mod reservations;
mod shutdown;
mod status;
#[cfg(test)]
mod tests;

use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::PathBuf;
use std::time::{Duration, Instant};

use hangar_core::WorkspaceId;
use hangar_core::device::capacity::{DeviceCost, HostResources, Pressure};
use hangar_core::device::{BackendReason, Conflict, DeviceInfo, DeviceKey, Platform, Target};

use policy::{Assignment, LeaseQueue, Limit};

pub use boot::{BootFailed, OpenStart, StepDone};
pub use leases::LeaseStart;
pub use policy::{LEASE_TIMEOUT, MAX_HOLD, RESERVATION_IDLE, REVOKE_GRACE};
pub use shutdown::{OwnShutdown, Replaced};
pub use status::{DeviceRow, DeviceStatus};

/// How often memory pressure, lease timeouts, the cap and open simulators are checked.
pub const MONITOR_TICK: Duration = Duration::from_secs(10);
/// How often a boot waiting for a free slot looks again.
pub const SLOT_POLL: Duration = Duration::from_secs(1);

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

/// Why a device could not be opened. Cloned to every waiter of the same boot.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum OpenError {
    /// The runtime guard refused to boot another device (message with the numbers).
    Refused(String),
    Failed(String),
}

impl OpenError {
    pub fn message(&self) -> &str {
        match self {
            Self::Refused(m) | Self::Failed(m) => m,
        }
    }

    /// What a waiter gets when its device goes away before it opened.
    pub(crate) fn stopped() -> Self {
        Self::Failed(STOPPED.into())
    }
}

/// The message of a waiter whose device left the pool.
const STOPPED: &str = "the device pool stopped";

/// An open device as the pool sees it (the app's session, a plain value in tests).
pub trait Session: Clone {
    fn info(&self) -> &DeviceInfo;
    /// Short backend name for logs.
    fn backend(&self) -> &'static str;
}

/// Identifies a waiter (a pending open or lease) in the commands that answer it.
pub type Token = u64;

/// A setting to save.
#[derive(Clone, Debug, PartialEq)]
pub enum Save {
    Assignment(WorkspaceId, Assignment),
    Limit(Limit),
    Base(Platform, String),
    LastTarget(DeviceKey),
    SingleDeviceAlertShown,
}

/// What a shutdown has to do.
#[derive(Clone, Debug)]
pub enum ShutdownKind<S> {
    /// Shut the open device down through its session, then drop the session.
    Session(S),
    /// Shut down a running device the pool has not opened.
    Device { platform: Platform, id: String },
}

/// A shutdown to run; report how it went with [`PoolMachine::shutdown_finished`].
#[derive(Clone, Debug)]
pub struct ShutdownJob<S> {
    pub key: PoolKey,
    pub kind: ShutdownKind<S>,
}

/// One I/O step of bringing a device up; report it with [`PoolMachine::step_done`].
#[derive(Clone, Debug, PartialEq)]
pub enum Step {
    /// Whether the target already runs (`DeviceCatalog::probe`).
    Probe(Target),
    /// Wait, then report [`StepDone::Waited`] (a full cap, or a failed eviction).
    Wait(Duration),
    /// Read the host, with the pressure override, for the boot guard.
    CheckHost,
    /// `DeviceCatalog::create_if_missing`.
    Create(Target),
    /// `DeviceCatalog::boot`; report its stages with [`PoolMachine::stage`].
    Boot(Target),
    /// `DeviceCatalog::open` and start a session.
    Open {
        platform: Platform,
        info: DeviceInfo,
        forced: Option<String>,
    },
}

#[derive(Clone, Debug, PartialEq)]
pub struct BringUp {
    pub key: PoolKey,
    /// Which bring-up of the key the step belongs to: results of an abandoned one are ignored.
    pub attempt: u64,
    pub step: Step,
}

/// What the pool asks of its driver.
#[derive(Debug)]
pub enum Command<S> {
    BringUp(BringUp),
    Shutdown(ShutdownJob<S>),
    /// Answer an open waiter.
    ReplyOpen {
        token: Token,
        result: Result<S, OpenError>,
    },
    /// Hand the lease on `key` to the waiter `token`, held by `holder`.
    GrantLease {
        token: Token,
        key: PoolKey,
        holder: WorkspaceId,
        /// The waiter is a call: release its hold with [`PoolMachine::release_lease`] when it
        /// ends. A bare reservation ([`PoolMachine::reserve`]) has nothing to release.
        hold: bool,
        /// Name of the workspace that installed the app now on the device, when not `holder`.
        stale_app: Option<String>,
    },
    FailLease {
        token: Token,
        message: String,
    },
    /// `holder` has held `key` for [`MAX_HOLD`] while others wait: its reservation is revoked
    /// in `ends_in` (once no call of it is in flight).
    WarnHolder {
        holder: WorkspaceId,
        key: PoolKey,
        ends_in: Duration,
    },
    /// `holder` lost its reservation on `key` (held too long, or taken over by the user); its
    /// next call queues.
    Revoked {
        holder: WorkspaceId,
        key: PoolKey,
    },
    Notice(NoticeLevel, String),
    Save(Save),
    /// Read the host and what an Android device of `android_base` costs; report with
    /// [`PoolMachine::host_detected`].
    DetectHost {
        android_base: Option<String>,
    },
    /// List what runs on every platform; report with [`PoolMachine::running_scanned`].
    ScanRunning,
}

/// The pool's saved settings, as read at launch.
#[derive(Debug, Default)]
pub struct Saved {
    pub limit: Option<Limit>,
    pub bases: BTreeMap<Platform, String>,
    pub alert_shown: bool,
    pub last_target: Option<DeviceKey>,
    pub assignments: HashMap<WorkspaceId, Assignment>,
}

/// Where a bring-up is.
#[derive(Clone, Debug, PartialEq)]
enum Phase {
    Probing,
    /// Waiting for a slot within the cap since `since`; `evicting` is shutting down for it.
    Claiming {
        since: Instant,
        evicting: Option<PoolKey>,
    },
    CheckingHost {
        cost: DeviceCost,
    },
    Creating,
    Booting,
    Opening {
        created: Option<String>,
    },
}

#[derive(Debug)]
struct Opening {
    stage: String,
    started: Instant,
    waiters: Vec<Token>,
    attempt: u64,
    phase: Phase,
}

#[derive(Debug)]
enum State<S> {
    Closed,
    Opening(Opening),
    Open {
        session: S,
        reason: Option<BackendReason>,
    },
    ShuttingDown,
}

#[derive(Debug)]
struct PoolDevice<S> {
    platform: Platform,
    name: String,
    target: Target,
    state: State<S>,
    /// Running (booted by the pool, found running, or claimed by a boot in progress).
    running: bool,
    last_used: u64,
    leases: LeaseQueue<WorkspaceId, Token>,
    error: Option<String>,
    /// The last boot was blocked by an instance running outside the pool.
    conflict: Option<Conflict>,
}

impl<S: Session> PoolDevice<S> {
    /// In use or about to be: a lease held (an idle reservation too, until it expires) or
    /// awaited, or a bring-up or shutdown in flight. Busy devices are never shut down to make
    /// room, so a reservation keeps the device booted for its holder.
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

pub struct PoolMachine<S> {
    limit: Limit,
    host: Option<HostResources>,
    host_error: Option<String>,
    pressure: Option<Pressure>,
    bases: BTreeMap<Platform, String>,
    android_cost: DeviceCost,
    assignments: HashMap<WorkspaceId, Assignment>,
    workspaces: HashMap<WorkspaceId, WorkspaceRef>,
    devices: HashMap<PoolKey, PoolDevice<S>>,
    /// Keys whose boot waits for a free slot.
    waiting_slot: HashSet<PoolKey>,
    focused: Option<WorkspaceId>,
    tick: u64,
    single_alert_shown: bool,
    last_target: Option<DeviceKey>,
    forced_backend: Option<String>,
    next_token: Token,
    out: Vec<Command<S>>,
}

impl<S: Session> PoolMachine<S> {
    /// `android_cost` until the Android base is known; `forced_backend` is
    /// `HANGAR_DEVICE_BACKEND`.
    pub fn new(android_cost: DeviceCost, forced_backend: Option<String>) -> Self {
        Self {
            limit: Limit::Auto,
            host: None,
            host_error: None,
            pressure: None,
            bases: BTreeMap::new(),
            android_cost,
            assignments: HashMap::new(),
            workspaces: HashMap::new(),
            devices: HashMap::new(),
            waiting_slot: HashSet::new(),
            focused: None,
            tick: 0,
            single_alert_shown: false,
            last_target: None,
            forced_backend,
            next_token: 0,
            out: Vec::new(),
        }
    }

    /// The commands queued since the last call, in order.
    pub fn take_commands(&mut self) -> Vec<Command<S>> {
        std::mem::take(&mut self.out)
    }

    fn token(&mut self) -> Token {
        self.next_token += 1;
        self.next_token
    }

    fn push(&mut self, command: Command<S>) {
        self.out.push(command);
    }

    fn save(&mut self, save: Save) {
        self.push(Command::Save(save));
    }

    fn notice(&mut self, level: NoticeLevel, message: String) {
        self.push(Command::Notice(level, message));
    }

    /// Drops the device; whoever waits for it is told the pool stopped.
    fn remove_device(&mut self, key: &PoolKey) {
        if let Some(mut device) = self.devices.remove(key) {
            let state = std::mem::replace(&mut device.state, State::Closed);
            self.abandon(state);
            for token in device.leases.take_waiting() {
                self.push(Command::FailLease {
                    token,
                    message: STOPPED.into(),
                });
            }
        }
    }

    /// Answers the waiters of a bring-up that will not finish.
    fn abandon(&mut self, state: State<S>) {
        if let State::Opening(opening) = state {
            for token in opening.waiters {
                self.push(Command::ReplyOpen {
                    token,
                    result: Err(OpenError::stopped()),
                });
            }
        }
    }
}
