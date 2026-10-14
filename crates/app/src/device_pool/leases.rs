//! Opening a workspace's device for the panel, or under a lease for a run or tool call.

use std::time::Instant;

use anyhow::anyhow;
use gpui_kit::*;
use hangar_core::WorkspaceId;
use tokio::sync::oneshot;

use crate::device_panel::Platform;
use crate::device_view::DeviceSession;

use super::policy::Acquire;
use super::{
    Acquired, DevicePool, Lease, LeaseReply, NoticeLevel, OpenError, PoolEvent, PoolKey, State,
};

impl DevicePool {
    /// Opens the workspace's device (booting it if needed) for the panel, without a lease.
    pub fn ensure(
        &mut self,
        ws: WorkspaceId,
        cx: &mut Context<Self>,
    ) -> Task<anyhow::Result<DeviceSession>> {
        let assignment = self.resolve_assignment(ws, cx);
        cx.spawn(async move |this, cx| {
            let a = assignment.await?;
            Self::wait_host(&this, cx).await;
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
            Self::wait_host(&this, cx).await;
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
            let fallback = this.update(cx, |pool, cx| {
                let other = pool.borrowable(a.platform, &key)?;
                let name = pool.devices.get(&other).map(|d| d.name.clone());
                cx.emit(PoolEvent::Notice(
                    NoticeLevel::Warning,
                    format!(
                        "{refused}. Waiting for {} instead, which another workspace also uses.",
                        name.unwrap_or_else(|| "a running device".into())
                    ),
                ));
                Some(other)
            })?;
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
    /// Open ones first, then ones opening or left running (which need no boot).
    fn borrowable(&self, platform: Platform, key: &PoolKey) -> Option<PoolKey> {
        self.devices
            .iter()
            .filter(|(k, d)| *k != key && d.platform == platform)
            .filter_map(|(k, d)| {
                let rank = match d.state {
                    State::Open { .. } => 0,
                    State::Opening { .. } => 1,
                    State::Closed if d.running => 2,
                    _ => return None,
                };
                Some(((rank, d.leases.waiting_len()), k))
            })
            .min_by_key(|(rank, _)| *rank)
            .map(|(_, k)| k.clone())
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

    pub(super) fn release_lease(
        &mut self,
        key: &PoolKey,
        holder: WorkspaceId,
        cx: &mut Context<Self>,
    ) {
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
}
