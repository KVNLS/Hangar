//! Shutting devices down: the cap, memory pressure, archived and deleted workspaces.

use std::time::Instant;

use anyhow::anyhow;
use gpui_kit::*;
use hangar_core::WorkspaceId;
use hangar_core::device::Pressure;
use hangar_core::ports::PortResult;
use hangar_usecases::settings::SettingsExt as _;

use crate::device_panel::Platform;
use crate::device_view::DeviceSession;
use crate::setting_keys;

use super::policy::{self, Assignment, LEASE_TIMEOUT};
use super::{DevicePool, NoticeLevel, PoolEvent, PoolKey, State, backend};
use crate::adapters;

impl DevicePool {
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
                        .spawn(async move { adapters::device_catalog(platform).shutdown(&id) })
                        .await
                        .map_err(anyhow::Error::from);
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

    /// Shuts down idle devices above the cap. The cap counts booted devices of every platform;
    /// with several platforms booted the tightest of their caps applies.
    pub(super) fn enforce_limit(&mut self, cx: &mut Context<Self>) {
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
            self.shutdown(&key, cx).detach();
        }
    }

    pub(super) fn on_monitor(&mut self, pressure: PortResult<Pressure>, cx: &mut Context<Self>) {
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

    /// UDIDs of the simulators the pool has open, to check they are still booted.
    pub(super) fn open_simulators(&self) -> Vec<String> {
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
    pub(super) fn close_gone_simulators(&mut self, booted: &[String], cx: &mut Context<Self>) {
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
        for name in &gone {
            cx.emit(PoolEvent::Notice(
                NoticeLevel::Warning,
                format!(
                    "{name} was shut down outside Hangar. It boots again the next time a \
                     workspace needs it."
                ),
            ));
        }
        if !gone.is_empty() {
            cx.notify();
        }
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
        let known = self.assignments.contains_key(&ws);
        let store = self.store.clone();
        cx.spawn(async move |this, cx| {
            // Right after launch the saved assignment may not be loaded yet.
            if !known && let Some(store) = store {
                let saved = cx
                    .background_executor()
                    .spawn(async move { store.get_setting(&setting_keys::device_assignment(ws)) })
                    .await?;
                if let Some(a) = saved {
                    this.update(cx, |pool, _| {
                        pool.assignments.entry(ws).or_insert(a);
                    })?;
                }
            }
            this.update(cx, |pool, cx| pool.shutdown_own(ws, cx))?.await
        })
    }

    fn shutdown_own(
        &mut self,
        ws: WorkspaceId,
        cx: &mut Context<Self>,
    ) -> Task<anyhow::Result<()>> {
        let key = PoolKey::Own(ws);
        let Some(device) = self.entry(&key) else {
            return Task::ready(Ok(()));
        };
        if matches!(device.state, State::Closed) && !device.running {
            // Possibly still running from an earlier launch of Hangar.
            let (platform, target) = (device.platform, device.target.clone());
            return cx.background_executor().spawn(async move {
                if adapters::device_catalog(platform).shutdown_if_running(&target)? {
                    tracing::info!(target: "device_pool", ?target, "device left from an earlier run shut down");
                }
                Ok(())
            });
        }
        self.shutdown(&key, cx)
    }

    /// Marks the workspaces' own devices that still run (from an earlier launch) as running, so
    /// they count against the cap and can be shut down when idle.
    pub(super) fn reconcile_running(&mut self, cx: &mut Context<Self>) {
        cx.spawn(async move |this, cx| {
            let running = cx
                .background_executor()
                .spawn(async { backend::running() })
                .await;
            let _ = this.update(cx, |pool, cx| {
                let owned: Vec<WorkspaceId> = pool.assignments.keys().copied().collect();
                for ws in owned {
                    let key = PoolKey::Own(ws);
                    let Some(device) = pool.entry(&key) else {
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
                        pool.devices.remove(&key);
                    }
                }
                pool.enforce_limit(cx);
                cx.notify();
            });
        })
        .detach();
    }

    /// The workspace is deleted: as [`Self::archive`], then its simulator (if the pool created
    /// one) and its assignment are deleted.
    pub fn delete(&mut self, ws: WorkspaceId, cx: &mut Context<Self>) -> Task<anyhow::Result<()>> {
        let shutdown = self.archive(ws, cx);
        let store = self.store.clone();
        cx.spawn(async move |this, cx| {
            if let Err(err) = shutdown.await {
                tracing::warn!(target: "device_pool", "shutdown before delete failed: {err:#}");
            }
            let assignment = this.update(cx, |pool, cx| {
                pool.devices.remove(&PoolKey::Own(ws));
                pool.workspaces.remove(&ws);
                cx.notify();
                pool.assignments.remove(&ws)
            })?;
            cx.background_executor()
                .spawn(async move {
                    let mut assignment = assignment;
                    if let Some(store) = store {
                        // Archived workspaces are not loaded into the pool: read theirs.
                        if assignment.is_none() {
                            assignment = store.get_setting(&setting_keys::device_assignment(ws))?;
                        }
                        store.delete_setting(&setting_keys::device_assignment(ws))?;
                    }
                    match assignment {
                        Some(Assignment {
                            platform: Platform::Ios,
                            own: Some(udid),
                            ..
                        }) => Ok(adapters::device_catalog(Platform::Ios).delete_clone(&udid)?),
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
            self.save(setting_keys::device_assignment(ws), a, cx);
        }
        cx.notify();
        cx.spawn(async move |_, cx| {
            let _ = shutdown.await;
            cx.background_executor()
                .spawn(async move { adapters::device_catalog(Platform::Ios).delete_clone(&udid) })
                .await?;
            Ok(())
        })
    }

    /// Shuts down the emulator that kept the workspace's device from booting (the base AVD
    /// running outside the pool), then starts the workspace's device.
    pub fn clear_conflict(
        &mut self,
        ws: WorkspaceId,
        cx: &mut Context<Self>,
    ) -> Task<anyhow::Result<DeviceSession>> {
        let Some(conflict) = self
            .key_for(ws)
            .and_then(|k| self.devices.get(&k))
            .and_then(|d| d.conflict.clone())
        else {
            return Task::ready(Err(anyhow!("nothing is in the way of this device")));
        };
        tracing::info!(target: "device_pool", workspace = %ws, serial = %conflict.serial, "shutting down the emulator in the way");
        cx.spawn(async move |this, cx| {
            cx.background_executor()
                .spawn(async move {
                    adapters::device_catalog(Platform::Android).clear_conflict(&conflict)
                })
                .await?;
            this.update(cx, |pool, cx| pool.ensure(ws, cx))?.await
        })
    }
}
