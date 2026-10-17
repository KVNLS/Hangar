//! `device_acquire` and `device_release`: an agent reserves its device for a sequence of calls
//! (or waits in line for it) and gives it back when done, and sees who shares it.

use std::time::{Duration, Instant};

use super::*;
use hangar_usecases::device_pool::SwitchBy;

use crate::device_panel::Platform;
use crate::mcp_host::{Acquisition, Released, acquisition, released};

/// How often `device_acquire` looks again while it waits for its turn.
const ACQUIRE_POLL: Duration = Duration::from_millis(250);

impl HangarApp {
    /// Reserves the workspace's device, or keeps its place in line and waits up to `wait` for
    /// its turn, keeping its place alive meanwhile. Never boots the device. A `platform` other
    /// than the attached one is switched to first ([`crate::device_pool::DevicePool::switch_platform`]).
    pub fn device_acquire(
        &mut self,
        ws: WorkspaceId,
        platform: Option<Platform>,
        wait: Duration,
        cx: &mut Context<Self>,
    ) -> Task<anyhow::Result<Acquisition>> {
        if !self.work.projects.is_active(ws) {
            return Task::ready(Err(anyhow!("unknown or archived workspace")));
        }
        self.track_workspace(ws, cx);
        let switch = match platform {
            Some(platform) => self.devices.pool.update(cx, |pool, cx| {
                pool.switch_platform(ws, platform, None, SwitchBy::Agent, cx)
            }),
            None => Task::ready(Ok(None)),
        };
        let pool = self.devices.pool.downgrade();
        cx.spawn(async move |_, cx| {
            let switched_from = switch.await?;
            pool.update(cx, |pool, cx| pool.reserve(ws, cx))?.await?;
            let deadline = Instant::now() + wait;
            loop {
                let sharing = pool
                    .update(cx, |pool, cx| {
                        pool.touch(ws, cx);
                        pool.sharing(ws)
                    })?
                    .ok_or_else(|| anyhow!("the pool does not know this workspace's device"))?;
                let now = Instant::now();
                if let Some(answer) = acquisition(&sharing, ws, now >= deadline, switched_from) {
                    return Ok(answer);
                }
                cx.background_executor()
                    .timer(ACQUIRE_POLL.min(deadline - now))
                    .await;
            }
        })
    }

    /// Ends the workspace's reservation (once its call in flight ends, if any) and takes it out
    /// of line (its waiting calls fail).
    pub fn device_release(
        &mut self,
        ws: WorkspaceId,
        cx: &mut Context<Self>,
    ) -> anyhow::Result<Released> {
        if !self.work.projects.is_active(ws) {
            return Err(anyhow!("unknown or archived workspace"));
        }
        Ok(self.devices.pool.update(cx, |pool, cx| {
            let before = pool.sharing(ws);
            let had_waiters = pool.release_reservation(ws, cx);
            released(before.as_ref(), ws, had_waiters)
        }))
    }
}
