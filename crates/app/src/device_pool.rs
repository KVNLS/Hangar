//! The device pool's GPUI shell. The pool itself (one device per workspace, leases, the cap,
//! memory pressure) is the sans-IO [`PoolMachine`] in `hangar_usecases::device_pool`; this
//! entity feeds it calls, timers and I/O results, and carries out the [`Command`]s it queues
//! through the device ports ([`crate::adapters::device_catalog`],
//! [`crate::adapters::host_monitor`]) on the background executor.
//!
//! All methods run on the GPUI thread.

mod assignments;
pub mod backend;
mod boot;
mod commands;
mod leases;
pub mod orphans;
mod shutdown;
mod status;

use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;

use anyhow::anyhow;
use gpui_kit::*;
use hangar_core::device::capacity::{HostResources, Pressure};
use hangar_core::ports::Store;
use hangar_core::{Setting, WorkspaceId};
use hangar_usecases::device_pool::{PoolMachine, Saved, Session, Token};
use hangar_usecases::settings::SettingsExt as _;
use tokio::sync::{mpsc, oneshot};

use crate::device_panel::Platform;
use crate::device_view::DeviceSession;
use crate::{adapters, setting_keys};
use policy::{Assignment, Limit};

pub use hangar_usecases::device_pool::{
    DeviceRow, NoticeLevel, OpenError, PoolKey, WorkspaceRef, policy,
};

/// What the device panel shows for a workspace.
pub type DeviceStatus<'a> = hangar_usecases::device_pool::DeviceStatus<'a, DeviceSession>;

impl Session for DeviceSession {
    fn info(&self) -> &hangar_core::device::DeviceInfo {
        DeviceSession::info(self)
    }

    fn backend(&self) -> &'static str {
        DeviceSession::backend(self)
    }
}

pub enum PoolEvent {
    /// Something the user should know (a refused boot, an automatic shutdown).
    Notice(NoticeLevel, String),
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

pub struct DevicePool {
    machine: PoolMachine<DeviceSession>,
    store: Option<Arc<dyn Store>>,
    release: mpsc::UnboundedSender<(PoolKey, WorkspaceId)>,
    /// Boot stages reported from the background executor: key, attempt, stage.
    stages: mpsc::UnboundedSender<(PoolKey, u64, String)>,
    open_waiters: HashMap<Token, oneshot::Sender<Opened>>,
    lease_waiters: HashMap<Token, LeaseReply>,
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
        let (stages, mut staged) = mpsc::unbounded_channel::<(PoolKey, u64, String)>();
        let stage_updates = cx.spawn(async move |this, cx| {
            while let Some((key, attempt, stage)) = staged.recv().await {
                if this
                    .update(cx, |pool, _| pool.machine.stage(&key, attempt, stage))
                    .is_err()
                {
                    break;
                }
            }
        });
        let monitor = cx.spawn(async move |this, cx| Self::monitor(this, cx).await);
        let forced_backend = std::env::var("HANGAR_DEVICE_BACKEND")
            .ok()
            .filter(|v| !v.is_empty());
        Self {
            machine: PoolMachine::new(
                adapters::device_catalog(Platform::Android).cost(None),
                forced_backend,
            ),
            store: None,
            release,
            stages,
            open_waiters: HashMap::new(),
            lease_waiters: HashMap::new(),
            _tasks: vec![releases, stage_updates, monitor],
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
        let ids: Vec<WorkspaceId> = workspaces.iter().map(|w| w.id).collect();
        for ws in workspaces {
            self.machine.track(ws);
        }
        cx.spawn(async move |this, cx| {
            let loaded = cx
                .background_executor()
                .spawn(async move { load_settings(&store, &ids) })
                .await;
            let _ = this.update(cx, |pool, cx| {
                pool.machine
                    .loaded(loaded.map_err(|e| format!("{e:#}")), limit_override);
                pool.run(cx);
                cx.notify();
            });
        })
        .detach();
    }

    /// Why Auto runs one device at a time (with the numbers), when it does.
    pub fn single_device_explanation(&self) -> Option<String> {
        self.machine.single_device_explanation()
    }

    pub fn limit(&self) -> Limit {
        self.machine.limit()
    }

    pub fn host(&self) -> Option<&HostResources> {
        self.machine.host()
    }

    pub fn pressure(&self) -> Option<Pressure> {
        self.machine.pressure()
    }

    pub fn base(&self, platform: Platform) -> Option<&str> {
        self.machine.base(platform)
    }

    /// What the host can run of this platform's devices (1 when it cannot be read).
    pub fn recommended(&self, platform: Platform) -> u32 {
        self.machine.recommended(platform)
    }

    /// The cap on booted devices when booting one of `platform`.
    pub fn limit_for(&self, platform: Platform) -> u32 {
        self.machine.limit_for(platform)
    }

    pub fn single_device(&self, platform: Platform) -> bool {
        self.machine.single_device(platform)
    }

    pub fn set_limit(&mut self, limit: Limit, cx: &mut Context<Self>) {
        self.machine.set_limit(limit);
        self.run(cx);
        cx.notify();
    }

    /// Base device used for this platform's workspaces when the user picked none.
    pub fn set_platform_base(&mut self, platform: Platform, id: String, cx: &mut Context<Self>) {
        self.machine.set_platform_base(platform, id);
        self.run(cx);
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
