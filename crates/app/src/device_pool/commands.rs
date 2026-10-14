//! Carrying out what the machine queues: answers to waiters, notices, saved settings, and host
//! and running-device scans on the background executor.

use std::time::Instant;

use anyhow::anyhow;
use gpui_kit::*;
use hangar_core::ports::PortError;
use hangar_usecases::device_pool::{Command, MONITOR_TICK, Save};
use hangar_usecases::settings::SettingsExt as _;

use crate::device_panel::Platform;
use crate::{adapters, setting_keys};

use super::{DevicePool, Lease, PoolEvent, backend};

/// A port error with its sources, as the pool shows and logs it.
pub(super) fn chain(err: PortError) -> String {
    format!("{:#}", anyhow::Error::from(err))
}

impl DevicePool {
    /// Runs the commands the machine queued.
    pub(super) fn run(&mut self, cx: &mut Context<Self>) {
        loop {
            let commands = self.machine.take_commands();
            if commands.is_empty() {
                return;
            }
            for command in commands {
                self.execute(command, cx);
            }
        }
    }

    fn execute(
        &mut self,
        command: Command<crate::device_view::DeviceSession>,
        cx: &mut Context<Self>,
    ) {
        match command {
            Command::BringUp(step) => self.bring_up(step, cx),
            Command::Shutdown(job) => self.run_shutdown(job, cx).detach(),
            Command::ReplyOpen { token, result } => {
                if let Some(waiter) = self.open_waiters.remove(&token) {
                    let _ = waiter.send(result);
                }
            }
            Command::GrantLease { token, key, holder } => {
                let lease = Lease {
                    key,
                    holder,
                    release: self.release.clone(),
                };
                // A waiter that gave up returns the lease, which releases it again.
                if let Some(waiter) = self.lease_waiters.remove(&token) {
                    let _ = waiter.send(Ok(lease));
                }
            }
            Command::FailLease { token, message } => {
                if let Some(waiter) = self.lease_waiters.remove(&token) {
                    let _ = waiter.send(Err(anyhow!(message)));
                }
            }
            Command::Notice(level, message) => cx.emit(PoolEvent::Notice(level, message)),
            Command::Save(save) => self.save_setting(save, cx),
            Command::DetectHost { android_base } => self.detect_host(android_base, cx),
            Command::ScanRunning => self.scan_running(cx),
        }
    }

    fn save_setting(&self, save: Save, cx: &mut Context<Self>) {
        match save {
            Save::Assignment(ws, a) => self.save(setting_keys::device_assignment(ws), a, cx),
            Save::Limit(limit) => self.save(setting_keys::DEVICE_LIMIT, limit, cx),
            Save::Base(platform, id) => self.save(setting_keys::device_base(platform), id, cx),
            Save::LastTarget(key) => self.save(setting_keys::LAST_DEVICE_TARGET, key, cx),
            Save::SingleDeviceAlertShown => {
                let Some(store) = self.store.clone() else {
                    return;
                };
                cx.background_executor()
                    .spawn(async move {
                        if let Err(err) =
                            store.set_setting(&setting_keys::SINGLE_DEVICE_ALERT_SHOWN, &true)
                        {
                            tracing::warn!(target: "device_pool", %err, "failed to save the alert flag");
                        }
                    })
                    .detach();
            }
        }
    }

    fn detect_host(&mut self, android_base: Option<String>, cx: &mut Context<Self>) {
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
                pool.machine
                    .host_detected(host.map_err(|e| e.to_string()), cost);
                pool.run(cx);
                cx.notify();
            });
        })
        .detach();
    }

    fn scan_running(&mut self, cx: &mut Context<Self>) {
        cx.spawn(async move |this, cx| {
            let running = cx
                .background_executor()
                .spawn(async { backend::running() })
                .await;
            let _ = this.update(cx, |pool, cx| {
                pool.machine.running_scanned(&running);
                pool.run(cx);
                cx.notify();
            });
        })
        .detach();
    }

    /// Every [`MONITOR_TICK`]: memory pressure, lease timeouts, the cap and open simulators.
    pub(super) async fn monitor(this: WeakEntity<Self>, cx: &mut AsyncApp) {
        loop {
            cx.background_executor().timer(MONITOR_TICK).await;
            let pressure = cx
                .background_executor()
                .spawn(async { adapters::host_monitor().pressure() })
                .await;
            let Ok(open) = this.update(cx, |pool, _| pool.machine.open_simulators()) else {
                break;
            };
            let booted = if open.is_empty() {
                None
            } else {
                Some(
                    cx.background_executor()
                        .spawn(async {
                            adapters::device_catalog(Platform::Ios)
                                .running()
                                .map(|running| running.into_iter().map(|r| r.id).collect())
                        })
                        .await,
                )
            };
            if this
                .update(cx, |pool, cx| {
                    pool.machine.monitor(
                        Instant::now(),
                        pressure.map_err(|e| e.to_string()),
                        booted.map(|b| b.map_err(|e| e.to_string())),
                    );
                    pool.run(cx);
                    cx.notify();
                })
                .is_err()
            {
                break;
            }
        }
    }
}
