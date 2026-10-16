//! `device_acquire` and `device_release`: an agent reserves its device for a sequence of calls
//! (or waits in line for it) and gives it back when done, and sees who shares it.

use std::time::{Duration, Instant};

use super::*;
use crate::device_pool::Sharing;
use crate::mcp_host::{Acquisition, Released, agent_sharing};

/// How often `device_acquire` looks again while it waits for its turn.
const ACQUIRE_POLL: Duration = Duration::from_millis(250);

impl HangarApp {
    /// Reserves the workspace's device, or keeps its place in line and waits up to `wait` for
    /// its turn, keeping its place alive meanwhile. Never boots the device.
    pub fn device_acquire(
        &mut self,
        ws: WorkspaceId,
        wait: Duration,
        cx: &mut Context<Self>,
    ) -> Task<anyhow::Result<Acquisition>> {
        if !self.work.projects.is_active(ws) {
            return Task::ready(Err(anyhow!("unknown or archived workspace")));
        }
        self.track_workspace(ws, cx);
        let reserve = self
            .devices
            .pool
            .update(cx, |pool, cx| pool.reserve(ws, cx));
        let pool = self.devices.pool.downgrade();
        cx.spawn(async move |_, cx| {
            reserve.await?;
            let deadline = Instant::now() + wait;
            loop {
                let sharing = pool
                    .update(cx, |pool, cx| {
                        pool.touch(ws, cx);
                        pool.sharing(ws)
                    })?
                    .ok_or_else(|| anyhow!("the pool does not know this workspace's device"))?;
                let granted = holds(&sharing, ws);
                let now = Instant::now();
                if granted || now >= deadline {
                    return Ok(Acquisition {
                        granted,
                        sharing: agent_sharing(&sharing, ws),
                    });
                }
                cx.background_executor()
                    .timer(ACQUIRE_POLL.min(deadline - now))
                    .await;
            }
        })
    }

    /// Ends the workspace's reservation (once its call in flight ends, if any).
    pub fn device_release(
        &mut self,
        ws: WorkspaceId,
        cx: &mut Context<Self>,
    ) -> anyhow::Result<Released> {
        if !self.work.projects.is_active(ws) {
            return Err(anyhow!("unknown or archived workspace"));
        }
        Ok(self.devices.pool.update(cx, |pool, cx| {
            let next = pool
                .sharing(ws)
                .filter(|s| holds(s, ws))
                .and_then(|s| s.queue.into_iter().find(|m| m.id != ws))
                .map(|m| m.name);
            let had_waiters = pool.release_reservation(ws, cx);
            Released {
                had_waiters,
                next: next.filter(|_| had_waiters),
            }
        }))
    }
}

/// Whether `ws` holds the device (a call or a reservation).
fn holds(s: &Sharing, ws: WorkspaceId) -> bool {
    s.holder.as_ref().is_some_and(|h| h.id == ws)
}
