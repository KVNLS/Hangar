//! Opening a workspace's device for the panel, or under a lease for a run or tool call.

use std::time::Instant;

use anyhow::anyhow;
use gpui_kit::*;
use hangar_core::WorkspaceId;
use hangar_usecases::device_pool::LeaseStart;
use tokio::sync::oneshot;

use crate::device_panel::Platform;
use crate::device_view::DeviceSession;

use super::{Acquired, DevicePool, Lease, NoticeLevel, OpenError, PoolEvent, PoolKey};

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
            let key = this.update(cx, |pool, _| pool.machine.key_of(ws, &a))?;
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
            let key = this.update(cx, |pool, _| pool.machine.key_of(ws, &a))?;
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
                Err(err @ OpenError::Moved { .. }) => return Err(anyhow!(err.message().to_owned())),
                Err(OpenError::Refused(why)) => why,
            };
            drop(lease);
            let fallback = this.update(cx, |pool, cx| {
                let other = pool.machine.borrowable(a.platform, &a.base, &key)?;
                let name = pool.machine.name_of(&other).map(str::to_owned);
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

    async fn lease(
        this: WeakEntity<Self>,
        key: PoolKey,
        ws: WorkspaceId,
        cx: &mut AsyncApp,
    ) -> anyhow::Result<Lease> {
        let granted = this.update(cx, |pool, cx| {
            let start = pool
                .machine
                .request_lease(&key, ws, Instant::now())
                .map_err(anyhow::Error::msg);
            cx.notify();
            anyhow::Ok(match start? {
                LeaseStart::Granted => Ok(Lease {
                    key: key.clone(),
                    holder: ws,
                    release: pool.release.clone(),
                }),
                LeaseStart::Queued(token) => {
                    let (tx, rx) = oneshot::channel();
                    pool.lease_waiters.insert(token, tx);
                    Err(rx)
                }
            })
        })??;
        match granted {
            Ok(lease) => Ok(lease),
            Err(rx) => rx.await.map_err(|_| anyhow!("the device pool stopped"))?,
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
