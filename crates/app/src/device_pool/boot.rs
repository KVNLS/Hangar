//! Bringing a device up: each step the machine asks for runs on the background executor and
//! reports back.

use std::time::{Duration, Instant};

use gpui_kit::*;
use hangar_core::WorkspaceId;
use hangar_core::device::Conflict;
use hangar_usecases::device_pool::{BootFailed, BringUp, OpenStart, Step, StepDone};
use tokio::sync::oneshot;

use crate::adapters;

use super::commands::chain;
use super::{DevicePool, OpenError, Opened, PoolKey, backend};

fn stopped() -> OpenError {
    OpenError::Failed("the device pool stopped".into())
}

impl DevicePool {
    /// Opens the device `key` (booting it if needed), or waits for the boot in progress. A call
    /// of `held_by` holding its lease gets [`OpenError::Moved`] once the lease was taken back.
    pub(super) async fn open(
        this: WeakEntity<Self>,
        key: PoolKey,
        held_by: Option<WorkspaceId>,
        cx: &mut AsyncApp,
    ) -> Opened {
        let started = this
            .update(cx, |pool, cx| {
                let now = Instant::now();
                let start = match held_by {
                    Some(ws) => pool.machine.open_held(&key, ws, now),
                    None => pool.machine.open(&key, now),
                };
                let started = match start {
                    OpenStart::Ready(result) => Ok(result),
                    OpenStart::Wait(token) => {
                        let (tx, rx) = oneshot::channel();
                        pool.open_waiters.insert(token, tx);
                        Err(rx)
                    }
                };
                pool.run(cx);
                cx.notify();
                started
            })
            .map_err(|_| stopped())?;
        match started {
            Ok(result) => result,
            Err(rx) => rx.await.unwrap_or_else(|_| Err(stopped())),
        }
    }

    pub(super) fn bring_up(&mut self, bring_up: BringUp, cx: &mut Context<Self>) {
        let BringUp { key, attempt, step } = bring_up;
        let stages = self.stages.clone();
        cx.spawn(async move |this, cx| {
            let background = cx.background_executor().clone();
            let done = match step {
                Step::Probe(target) => {
                    StepDone::Probed(
                        background
                            .spawn(async move {
                                adapters::device_catalog(target.platform()).probe(&target)
                            })
                            .await
                            .map_err(chain),
                    )
                }
                Step::Wait(delay) => {
                    background.timer(delay).await;
                    StepDone::Waited
                }
                Step::CheckHost => StepDone::HostChecked(
                    background
                        .spawn(async {
                            // `HANGAR_PRESSURE` (tests) applies to the guard too.
                            let monitor = adapters::host_monitor();
                            monitor.detect().map(|mut host| {
                                if let Ok(p) = monitor.pressure() {
                                    host.pressure = p;
                                }
                                host
                            })
                        })
                        .await
                        .map_err(|e| e.to_string()),
                ),
                Step::Create(target) => StepDone::Created(
                    background
                        .spawn(async move {
                            adapters::device_catalog(target.platform()).create_if_missing(&target)
                        })
                        .await
                        .map_err(chain),
                ),
                Step::Boot(target) => {
                    let stage_key = key.clone();
                    StepDone::Booted(
                        background
                            .spawn(async move {
                                adapters::device_catalog(target.platform()).boot(&target, &|s| {
                                    let _ = stages.send((stage_key.clone(), attempt, s));
                                })
                            })
                            .await
                            .map_err(|err| BootFailed {
                                conflict: err.inner().downcast_ref::<Conflict>().cloned(),
                                message: chain(err),
                            }),
                    )
                }
                Step::Open {
                    platform,
                    info,
                    forced,
                } => StepDone::Opened(
                    background
                        .spawn(async move { backend::open(platform, info, forced.as_deref()) })
                        .await
                        .map_err(|e| format!("{e:#}")),
                ),
            };
            let _ = this.update(cx, |pool, cx| {
                pool.machine.step_done(&key, attempt, done, Instant::now());
                pool.run(cx);
                cx.notify();
            });
        })
        .detach();
    }

    /// Waits (briefly) for host detection, so the first calls after launch pick the right mode.
    pub(super) async fn wait_host(this: &WeakEntity<Self>, cx: &mut AsyncApp) {
        let deadline = Instant::now() + Duration::from_secs(10);
        while Instant::now() < deadline {
            if this
                .read_with(cx, |pool, _| pool.machine.host_known())
                .unwrap_or(true)
            {
                return;
            }
            cx.background_executor()
                .timer(Duration::from_millis(50))
                .await;
        }
    }
}
