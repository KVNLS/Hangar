//! Bringing a device up: creating the workspace's own device, booting it within the cap.

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use gpui_kit::*;
use hangar_core::WorkspaceId;
use hangar_devices::capacity::{self, HostResources};
use tokio::sync::oneshot;

use crate::device_panel::{BackendReason, Platform};
use crate::device_view::DeviceSession;
use crate::setting_keys;

use super::backend::{self, Target};
use super::policy::{self, Capacity, LEASE_TIMEOUT, Slot};
use super::{DevicePool, OpenError, Opened, PoolKey, SLOT_POLL, State};

impl DevicePool {
    pub(super) fn slots(&self, except: Option<&PoolKey>) -> Vec<Slot<PoolKey>> {
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

    pub(super) async fn open(this: WeakEntity<Self>, key: PoolKey, cx: &mut AsyncApp) -> Opened {
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
            device.conflict = None;
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
                    .spawn(async {
                        // `HANGAR_PRESSURE` (tests) applies to the guard too.
                        HostResources::detect().map(|mut host| {
                            if let Ok(p) = capacity::pressure_with_override() {
                                host.pressure = p;
                            }
                            host
                        })
                    })
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
                        "Not booting another {} device: {refusal} ({numbers}; Devices at once \
                         is in Settings › Devices)",
                        platform.label()
                    )));
                }
                tracing::info!(target: "device_pool", ?key, "booting");
                let mut target = target;
                if matches!(target, Target::IosOwn { .. }) {
                    set_stage("creating the simulator");
                    let create_target = target.clone();
                    let created = cx
                        .background_executor()
                        .spawn(async move { backend::create_if_missing(&create_target) })
                        .await
                        .map_err(failed)?;
                    if let (Some(udid), Target::IosOwn { udid: own, .. }) = (created, &mut target) {
                        *own = Some(udid.clone());
                        let recorded = target.clone();
                        this.update(cx, |pool, cx| {
                            if let Some(d) = pool.devices.get_mut(key) {
                                d.target = recorded;
                            }
                            if let PoolKey::Own(ws) = key {
                                pool.record_own(*ws, Some(udid.clone()), udid, cx);
                            }
                        })
                        .map_err(gone)?;
                    }
                }
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
                        let conflict = err.downcast_ref::<backend::Conflict>().cloned();
                        let _ = this.update(cx, |pool, _| {
                            if let Some(d) = pool.devices.get_mut(key) {
                                d.running = false;
                                d.conflict = conflict;
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
            self.save(setting_keys::device_assignment(ws), a, cx);
        }
    }

    /// Whether host detection finished (the cap is known).
    pub(super) fn host_known(&self) -> bool {
        self.host.is_some() || self.host_error.is_some()
    }

    /// Waits (briefly) for host detection, so the first calls after launch pick the right mode.
    pub(super) async fn wait_host(this: &WeakEntity<Self>, cx: &mut AsyncApp) {
        let deadline = Instant::now() + Duration::from_secs(10);
        while Instant::now() < deadline {
            if this
                .read_with(cx, |pool, _| pool.host_known())
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
