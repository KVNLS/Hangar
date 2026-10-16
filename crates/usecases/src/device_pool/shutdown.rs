//! Shutting devices down: the cap, memory pressure, simulators gone, archived and deleted
//! workspaces, devices running from an earlier launch.

use std::time::Instant;

use hangar_core::WorkspaceId;
use hangar_core::device::{Conflict, Platform, Pressure, RunningSet, Target};

use super::policy::{self, Assignment};
use super::{
    Command, NoticeLevel, PoolKey, PoolMachine, Save, Session, ShutdownJob, ShutdownKind, State,
};

/// What shutting a workspace's own device down takes ([`PoolMachine::shutdown_own`]).
#[derive(Debug)]
pub enum OwnShutdown<S> {
    Nothing,
    /// Not opened in this run: shut it down if it still runs from an earlier launch.
    IfRunning {
        platform: Platform,
        target: Target,
    },
    Job(ShutdownJob<S>),
}

/// A workspace's own device replaced by a new assignment: shut it down, then delete the
/// simulator the pool made for it.
#[derive(Debug)]
pub struct Replaced<S> {
    pub shutdown: Option<ShutdownJob<S>>,
    pub clone: Option<String>,
}

impl<S: Session> PoolMachine<S> {
    /// Shuts a pool device down. `None` when there is nothing to wait for (not running, or
    /// already starting or stopping); otherwise run the job and report it with
    /// [`Self::shutdown_finished`].
    pub fn shutdown(&mut self, key: &PoolKey) -> Option<ShutdownJob<S>> {
        let device = self.devices.get_mut(key)?;
        let platform = device.platform;
        match std::mem::replace(&mut device.state, State::ShuttingDown) {
            State::Open { session, .. } => {
                tracing::info!(target: "device_pool", ?key, name = %device.name, "shutting down");
                Some(ShutdownJob {
                    key: key.clone(),
                    kind: ShutdownKind::Session(session),
                })
            }
            State::Closed if device.running => {
                let Some(id) = device.device_id() else {
                    device.state = State::Closed;
                    device.running = false;
                    return None;
                };
                tracing::info!(target: "device_pool", ?key, %id, "shutting down (not open)");
                Some(ShutdownJob {
                    key: key.clone(),
                    kind: ShutdownKind::Device { platform, id },
                })
            }
            other => {
                device.state = other;
                None
            }
        }
    }

    fn shutdown_detached(&mut self, key: &PoolKey) {
        if let Some(job) = self.shutdown(key) {
            self.push(Command::Shutdown(job));
        }
    }

    /// How the shutdown of `key` went (`Err` holds the message).
    pub fn shutdown_finished(&mut self, key: &PoolKey, result: Result<(), String>, now: Instant) {
        if let Some(device) = self.devices.get_mut(key) {
            let before = std::mem::replace(&mut device.state, State::Closed);
            match &result {
                Ok(()) => {
                    device.running = false;
                    tracing::info!(target: "device_pool", ?key, "device shut down");
                }
                Err(err) => {
                    tracing::warn!(target: "device_pool", ?key, "shutdown failed: {err}");
                    device.error = Some(format!("Shutdown failed: {err}"));
                }
            }
            self.abandon(before);
        }
        self.evicted(key, &result, now);
    }

    /// Shuts down idle devices above the cap. The cap counts booted devices of every platform;
    /// with several platforms booted the tightest of their caps applies.
    pub(super) fn enforce_limit(&mut self) {
        if !self.host_known() {
            return;
        }
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
            self.shutdown_detached(&key);
        }
    }

    /// The periodic check ([`super::MONITOR_TICK`]): lease timeouts, reservations, memory
    /// pressure (`Err` when unreadable), the cap, and `booted` simulators (read when
    /// [`Self::open_simulators`] was not empty).
    pub fn monitor(
        &mut self,
        now: Instant,
        pressure: Result<Pressure, String>,
        booted: Option<Result<Vec<String>, String>>,
    ) {
        self.expire_leases(now);
        self.expire_reservations(now);
        match pressure {
            Ok(pressure) => {
                if self.pressure != Some(pressure) {
                    tracing::info!(target: "device_pool", %pressure, "memory pressure");
                    self.pressure = Some(pressure);
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
                    self.notice(
                        NoticeLevel::Warning,
                        format!(
                            "Memory pressure is critical: shut down {name}, which nothing was \
                             using. Devices in use by a run or an agent are left alone."
                        ),
                    );
                    self.shutdown_detached(&victim);
                }
            }
            Err(err) => {
                tracing::debug!(target: "device_pool", "memory pressure unavailable: {err}")
            }
        }
        self.enforce_limit();
        match booted {
            Some(Ok(booted)) => self.close_gone_simulators(&booted),
            Some(Err(err)) => {
                tracing::debug!(target: "device_pool", "cannot list booted simulators: {err}")
            }
            None => {}
        }
    }

    /// UDIDs of the simulators the pool has open, to check they are still booted.
    pub fn open_simulators(&self) -> Vec<String> {
        self.devices
            .values()
            .filter(|d| d.platform == Platform::Ios)
            .filter_map(|d| match &d.state {
                State::Open { session, .. } => Some(session.info().udid.clone()),
                _ => None,
            })
            .collect()
    }

    /// Closes the open simulators missing from `booted`: something outside Hangar shut them
    /// down, and their session would keep serving its last frame. The next lease boots them.
    fn close_gone_simulators(&mut self, booted: &[String]) {
        let mut gone = Vec::new();
        for (key, device) in &mut self.devices {
            if device.platform != Platform::Ios {
                continue;
            }
            let State::Open { session, .. } = &device.state else {
                continue;
            };
            if booted.contains(&session.info().udid) {
                continue;
            }
            tracing::warn!(target: "device_pool", ?key, name = %device.name, "simulator shut down outside Hangar");
            device.state = State::Closed;
            device.running = false;
            device.error = Some("shut down outside Hangar".to_owned());
            gone.push(device.name.clone());
        }
        for name in gone {
            self.notice(
                NoticeLevel::Warning,
                format!(
                    "{name} was shut down outside Hangar. It boots again the next time a \
                     workspace needs it."
                ),
            );
        }
    }

    /// The workspace is archived: its waiters give up and its reservations end. Whether its
    /// assignment is known; the caller reads an unknown one ([`Self::adopt_assignment`]) before
    /// [`Self::shutdown_own`].
    pub fn archive(&mut self, ws: WorkspaceId, now: Instant) -> bool {
        let mut cancelled = Vec::new();
        for device in self.devices.values_mut() {
            cancelled.extend(device.leases.cancel(&ws));
        }
        for token in cancelled {
            self.push(Command::FailLease {
                token,
                message: "the workspace was closed".into(),
            });
        }
        self.release_reservation(ws, now);
        if self.focused == Some(ws) {
            self.focused = None;
        }
        self.assignments.contains_key(&ws)
    }

    /// A saved assignment read after launch; one already known wins.
    pub fn adopt_assignment(&mut self, ws: WorkspaceId, assignment: Assignment) {
        self.assignments.entry(ws).or_insert(assignment);
    }

    /// Shuts the workspace's own device down.
    pub fn shutdown_own(&mut self, ws: WorkspaceId) -> OwnShutdown<S> {
        let key = PoolKey::Own(ws);
        let Some(device) = self.entry(&key) else {
            return OwnShutdown::Nothing;
        };
        if matches!(device.state, State::Closed) && !device.running {
            // Possibly still running from an earlier launch of Hangar.
            return OwnShutdown::IfRunning {
                platform: device.platform,
                target: device.target.clone(),
            };
        }
        match self.shutdown(&key) {
            Some(job) => OwnShutdown::Job(job),
            None => OwnShutdown::Nothing,
        }
    }

    /// Marks the workspaces' own devices that still run (from an earlier launch) as running, so
    /// they count against the cap and can be shut down when idle.
    pub fn running_scanned(&mut self, running: &RunningSet) {
        let owned: Vec<WorkspaceId> = self.assignments.keys().copied().collect();
        for ws in owned {
            let key = PoolKey::Own(ws);
            let Some(device) = self.entry(&key) else {
                continue;
            };
            if running.find(&device.target).is_some() && !device.running {
                device.running = true;
                tracing::info!(target: "device_pool", workspace = %ws, name = %device.name, "own device still running from an earlier launch");
            } else if !device.running
                && matches!(device.state, State::Closed)
                && device.leases.is_idle()
                // A failed start keeps its entry: the panel shows why and how to fix it.
                && device.error.is_none()
                && device.conflict.is_none()
            {
                self.remove_device(&key);
            }
        }
        self.enforce_limit();
    }

    /// The workspace is deleted (after [`Self::archive`] and its shutdown): forgets it and
    /// returns its assignment, if known.
    pub fn forget(&mut self, ws: WorkspaceId) -> Option<Assignment> {
        self.remove_device(&PoolKey::Own(ws));
        self.workspaces.remove(&ws);
        self.assignments.remove(&ws)
    }

    /// Settings › Devices: forgets the simulator the pool created for a workspace. Returns its
    /// UDID to delete once the shutdown (if any) is over. The workspace gets a new one the next
    /// time it needs a device.
    pub fn delete_clone(
        &mut self,
        key: &PoolKey,
    ) -> Result<(String, Option<ShutdownJob<S>>), String> {
        let PoolKey::Own(ws) = key else {
            return Err("only devices Hangar created can be deleted".into());
        };
        let ws = *ws;
        let Some(udid) = self
            .assignments
            .get(&ws)
            .filter(|a| a.platform == Platform::Ios)
            .and_then(|a| a.own.clone())
        else {
            return Err("no simulator was created for this workspace".into());
        };
        let shutdown = self.shutdown(key);
        self.remove_device(key);
        if let Some(a) = self.assignments.get_mut(&ws) {
            a.own = None;
            let a = a.clone();
            self.save(Save::Assignment(ws, a));
        }
        Ok((udid, shutdown))
    }

    /// The emulator that kept the workspace's device from booting, if any.
    pub fn conflict(&self, ws: WorkspaceId) -> Option<Conflict> {
        self.key_for(ws)
            .and_then(|k| self.devices.get(&k))
            .and_then(|d| d.conflict.clone())
    }
}
