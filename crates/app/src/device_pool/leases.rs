//! Opening a workspace's device for the panel, or under a lease for a run or tool call.

use std::time::{Duration, Instant};

use anyhow::anyhow;
use gpui_kit::*;
use hangar_core::WorkspaceId;
use hangar_usecases::device_pool::{DeviceStart, LeaseStart};
use tokio::sync::oneshot;

use crate::device_panel::Platform;
use crate::device_view::DeviceSession;

use super::{Acquired, DevicePool, Granted, Lease, OpenError, PoolKey, StillQueued};

/// How many times a call asks for a device again after its boot was refused or moved.
const MAX_ATTEMPTS: usize = 4;

/// A device the machine chose for a call: its lease, or the wait for it.
enum Pending {
    Granted(Granted),
    Queued(oneshot::Receiver<anyhow::Result<Granted>>),
}

impl DevicePool {
    /// Opens the device the workspace uses now (the one it shares, else its own; booting it if
    /// needed) for the panel, without a lease.
    pub fn ensure(
        &mut self,
        ws: WorkspaceId,
        cx: &mut Context<Self>,
    ) -> Task<anyhow::Result<DeviceSession>> {
        let assignment = self.resolve_assignment(ws, cx);
        cx.spawn(async move |this, cx| {
            let a = assignment.await?;
            Self::wait_host(&this, cx).await;
            let key = this.update(cx, |pool, _| {
                pool.machine
                    .key_for(ws)
                    .unwrap_or_else(|| pool.machine.key_of(ws, &a))
            })?;
            Self::open(this, key, cx)
                .await
                .map_err(|e| anyhow!(e.message().to_owned()))
        })
    }

    /// Opens a device for a call of the workspace under a lease: the machine picks it (its own
    /// device, or at the cap a compatible running one it shares) and the call waits in line
    /// for it. A refused boot moves the call to a running device of its class; a boot that
    /// waits for a slot may move it too. `budget`: how long the call may wait in line, after
    /// which it fails with [`StillQueued`] and keeps its place.
    pub fn acquire(
        &mut self,
        ws: WorkspaceId,
        budget: Option<Duration>,
        cx: &mut Context<Self>,
    ) -> Task<anyhow::Result<Acquired>> {
        let assignment = self.resolve_assignment(ws, cx);
        cx.spawn(async move |this, cx| {
            let a = assignment.await?;
            Self::wait_host(&this, cx).await;
            let deadline = budget.map(|b| Instant::now() + b);
            let avd = (a.platform == Platform::Android).then(|| a.base.clone());
            let mut pending = this.update(cx, |pool, cx| {
                let start = pool.machine.request_device(ws, Instant::now());
                pool.pending(ws, start, cx)
            })??;
            for _ in 0..MAX_ATTEMPTS {
                let Granted { lease, stale_app } = Self::granted(ws, pending, deadline, cx)
                    .await?;
                let key = lease.key().clone();
                let refused = match Self::open(this.clone(), key.clone(), cx).await {
                    Ok(session) => {
                        let (borrowed, shared_with) = this.update(cx, |pool, cx| {
                            pool.tell_stale(ws, &key, stale_app.as_deref(), cx);
                            let shared_with = pool
                                .machine
                                .sharing(ws, Instant::now())
                                .filter(|s| s.key == key)
                                .map(|s| s.shared_with)
                                .unwrap_or_default();
                            (key != pool.machine.key_of(ws, &a), shared_with)
                        })?;
                        return Ok(Acquired {
                            session,
                            lease,
                            platform: a.platform,
                            avd,
                            borrowed,
                            shared_with,
                            stale_app,
                        });
                    }
                    Err(OpenError::Failed(err)) => return Err(anyhow!(err)),
                    Err(OpenError::Moved { to, .. }) => {
                        tracing::info!(target: "device_pool", workspace = %ws, from = ?key, ?to, "device moved, asking again");
                        lease.disarm();
                        pending = this.update(cx, |pool, cx| {
                            let start = pool.machine.request_device(ws, Instant::now());
                            pool.pending(ws, start, cx)
                        })??;
                        continue;
                    }
                    Err(OpenError::Refused(why)) => why,
                };
                lease.disarm();
                let moved = this.update(cx, |pool, cx| {
                    let moved = pool.machine.boot_refused(ws, &key, Instant::now());
                    match moved {
                        Ok(Some(start)) => pool.pending(ws, Ok(start), cx).map(Some),
                        Ok(None) => {
                            pool.run(cx);
                            Ok(None)
                        }
                        Err(err) => {
                            pool.run(cx);
                            Err(anyhow!(err))
                        }
                    }
                })??;
                let Some(next) = moved else {
                    return Err(anyhow!(
                        "{refused}. No other {} device is running to share.",
                        a.platform.label()
                    ));
                };
                tracing::info!(target: "device_pool", workspace = %ws, from = ?key, "boot refused, sharing a running device");
                pending = next;
            }
            Err(anyhow!(
                "the device kept moving while it was being opened; try again"
            ))
        })
    }

    /// Runs what `start` queued and, in the same update, takes its lease or registers its
    /// waiter, so that neither a grant nor a hold can be lost if the call goes away.
    fn pending(
        &mut self,
        ws: WorkspaceId,
        start: Result<DeviceStart, String>,
        cx: &mut Context<Self>,
    ) -> anyhow::Result<Pending> {
        let pending = start.map(|start| match start.lease {
            LeaseStart::Granted => Pending::Granted(Granted {
                lease: self.lease(start.key, ws, true),
                stale_app: start.stale_app,
            }),
            LeaseStart::Queued(token) => {
                let (tx, rx) = oneshot::channel();
                self.lease_waiters.insert(token, tx);
                Pending::Queued(rx)
            }
        });
        self.run(cx);
        cx.notify();
        pending.map_err(anyhow::Error::msg)
    }

    /// The lease of `pending`, once granted. Past `deadline` the waiter goes away and the
    /// ticket stays in line: a later grant finds no waiter and returns the lease, so the
    /// workspace then holds a bare reservation.
    async fn granted(
        ws: WorkspaceId,
        pending: Pending,
        deadline: Option<Instant>,
        cx: &mut AsyncApp,
    ) -> anyhow::Result<Granted> {
        let rx = match pending {
            Pending::Granted(granted) => return Ok(granted),
            Pending::Queued(rx) => rx,
        };
        let reply = match deadline {
            None => rx.await,
            Some(deadline) => {
                let left = deadline.saturating_duration_since(Instant::now());
                match rx.with_timeout(left, cx.background_executor()).await {
                    Ok(reply) => reply,
                    Err(_) => {
                        tracing::info!(target: "device_pool", workspace = %ws, "call budget spent in line; its place is kept");
                        return Err(StillQueued.into());
                    }
                }
            }
        };
        reply.map_err(|_| anyhow!("the device pool stopped"))?
    }

    pub(super) fn lease(&self, key: PoolKey, holder: WorkspaceId, hold: bool) -> Lease {
        Lease {
            key,
            holder,
            hold,
            release: self.release.clone(),
        }
    }

    pub(super) fn release_lease(
        &mut self,
        key: &PoolKey,
        holder: WorkspaceId,
        cx: &mut Context<Self>,
    ) {
        self.machine.release_lease(key, holder, Instant::now());
        self.run(cx);
        cx.notify();
    }

    /// Whether the workspace waits for a device (lease queue or a free slot).
    pub fn is_waiting(&self, ws: WorkspaceId) -> bool {
        self.machine.is_waiting(ws)
    }
}
