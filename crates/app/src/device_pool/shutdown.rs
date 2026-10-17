//! Shutting devices down: on request, for the cap and memory pressure, archived and deleted
//! workspaces, deleted clones and emulators in the way.

use std::time::Instant;

use anyhow::anyhow;
use gpui_kit::*;
use hangar_core::WorkspaceId;
use hangar_usecases::device_pool::{Forgotten, OwnShutdown, ShutdownJob, ShutdownKind};
use hangar_usecases::settings::SettingsExt as _;

use crate::device_panel::Platform;
use crate::device_view::DeviceSession;
use crate::{adapters, setting_keys};

use super::policy::Assignment;
use super::{DevicePool, PoolKey};

/// A shutdown under way: the open session's acknowledgement, or a device to shut down.
enum Pending {
    Session {
        done: tokio::sync::oneshot::Receiver<Result<(), String>>,
        session: DeviceSession,
    },
    Device {
        platform: Platform,
        id: String,
    },
}

impl DevicePool {
    /// Shuts a pool device down (no-op when it is not running).
    pub fn shutdown(&mut self, key: &PoolKey, cx: &mut Context<Self>) -> Task<anyhow::Result<()>> {
        match self.machine.shutdown(key) {
            Some(job) => {
                cx.notify();
                self.run_shutdown(job, cx)
            }
            None => Task::ready(Ok(())),
        }
    }

    /// Runs a shutdown the machine decided on and reports the result to it.
    pub(super) fn run_shutdown(
        &mut self,
        job: ShutdownJob<DeviceSession>,
        cx: &mut Context<Self>,
    ) -> Task<anyhow::Result<()>> {
        let ShutdownJob { key, kind } = job;
        let pending = match kind {
            ShutdownKind::Session(session) => Pending::Session {
                done: session.shutdown_acked(),
                session,
            },
            ShutdownKind::Device { platform, id } => Pending::Device { platform, id },
        };
        cx.spawn(async move |this, cx| {
            let result = match pending {
                Pending::Session { done, session } => {
                    let result = done
                        .await
                        .map_err(|_| anyhow!("the device stopped before it shut down"))
                        .and_then(|r| r.map_err(anyhow::Error::msg));
                    drop(session);
                    result
                }
                Pending::Device { platform, id } => cx
                    .background_executor()
                    .spawn(async move { adapters::device_catalog(platform).shutdown(&id) })
                    .await
                    .map_err(anyhow::Error::from),
            };
            let reported = result.as_ref().map(|_| ()).map_err(|e| format!("{e:#}"));
            this.update(cx, |pool, cx| {
                pool.machine
                    .shutdown_finished(&key, reported, Instant::now());
                pool.run(cx);
                cx.notify();
            })?;
            result
        })
    }

    /// The workspace is archived: its waiters give up and its own device shuts down.
    pub fn archive(&mut self, ws: WorkspaceId, cx: &mut Context<Self>) -> Task<anyhow::Result<()>> {
        let known = self.machine.archive(ws, Instant::now());
        let parked_known = self.machine.parked(ws).is_some();
        self.run(cx);
        cx.notify();
        let store = self.store.clone();
        cx.spawn(async move |this, cx| {
            // Right after launch the saved assignments may not be loaded yet; a parked device
            // may still run from an earlier launch.
            if (!known || !parked_known)
                && let Some(store) = store
            {
                let (active, parked) = cx
                    .background_executor()
                    .spawn(async move {
                        let active = match known {
                            true => None,
                            false => store.get_setting(&setting_keys::device_assignment(ws))?,
                        };
                        let parked = match parked_known {
                            true => None,
                            false => store.get_setting(&setting_keys::device_parked(ws))?,
                        };
                        anyhow::Ok((active, parked))
                    })
                    .await?;
                this.update(cx, |pool, cx| {
                    pool.machine.adopt(ws, active, parked);
                    pool.run(cx);
                })?;
            }
            this.update(cx, |pool, cx| pool.shutdown_own(ws, cx))?.await
        })
    }

    fn shutdown_own(
        &mut self,
        ws: WorkspaceId,
        cx: &mut Context<Self>,
    ) -> Task<anyhow::Result<()>> {
        let tasks: Vec<Task<anyhow::Result<()>>> = self
            .machine
            .shutdown_own(ws)
            .into_iter()
            .map(|own| match own {
                OwnShutdown::IfRunning { platform, target } => {
                    cx.background_executor().spawn(async move {
                        if adapters::device_catalog(platform).shutdown_if_running(&target)? {
                            tracing::info!(target: "device_pool", ?target, "device left from an earlier run shut down");
                        }
                        Ok(())
                    })
                }
                OwnShutdown::Job(job) => {
                    cx.notify();
                    self.run_shutdown(job, cx)
                }
            })
            .collect();
        cx.spawn(async move |_, _| {
            let mut result = Ok(());
            for task in tasks {
                if let Err(err) = task.await {
                    result = Err(err);
                }
            }
            result
        })
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
            let forgotten = this.update(cx, |pool, cx| {
                let forgotten = pool.machine.forget(ws);
                pool.run(cx);
                cx.notify();
                forgotten
            })?;
            cx.background_executor()
                .spawn(async move {
                    let Forgotten {
                        mut active,
                        mut parked,
                    } = forgotten;
                    if let Some(store) = store {
                        // Archived workspaces are not loaded into the pool: read theirs.
                        if active.is_none() {
                            active = store.get_setting(&setting_keys::device_assignment(ws))?;
                        }
                        if parked.is_none() {
                            parked = store.get_setting(&setting_keys::device_parked(ws))?;
                        }
                        store.delete_setting(&setting_keys::device_assignment(ws))?;
                        store.delete_setting(&setting_keys::device_parked(ws))?;
                    }
                    for a in [active, parked].into_iter().flatten() {
                        if let Assignment {
                            platform: Platform::Ios,
                            own: Some(udid),
                            ..
                        } = a
                        {
                            adapters::device_catalog(Platform::Ios).delete_clone(&udid)?;
                        }
                    }
                    Ok(())
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
        let (udid, shutdown) = match self.machine.delete_clone(key) {
            Ok(found) => found,
            Err(err) => return Task::ready(Err(anyhow!(err))),
        };
        let shutdown = match shutdown {
            Some(job) => self.run_shutdown(job, cx),
            None => Task::ready(Ok(())),
        };
        self.run(cx);
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
        let Some(conflict) = self.machine.conflict(ws) else {
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
