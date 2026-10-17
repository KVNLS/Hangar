//! Taking turns on a shared device: explicit reservations (an agent's `device_acquire` and
//! `device_release`), activity that keeps them, the user's take-over, and what an agent is told
//! about its turn.

use std::time::{Duration, Instant};

use gpui_kit::*;
use hangar_core::WorkspaceId;
use hangar_usecases::device_pool::LeaseStart;

use crate::device_panel::Platform;

use super::{DevicePool, PoolEvent, PoolKey, Sharing};

impl DevicePool {
    /// Reserves the device the workspace uses (no call, nothing to release; it lasts until it
    /// expires idle, is released or revoked), or keeps its place when it already waits.
    /// Resolves to whether the workspace holds it now; when queued, the caller polls
    /// [`Self::sharing`] (with [`Self::touch`]) for its turn.
    pub fn reserve(
        &mut self,
        ws: WorkspaceId,
        cx: &mut Context<Self>,
    ) -> Task<anyhow::Result<bool>> {
        let assignment = self.resolve_assignment(ws, cx);
        cx.spawn(async move |this, cx| {
            assignment.await?;
            Self::wait_host(&this, cx).await;
            this.update(cx, |pool, cx| {
                let now = Instant::now();
                pool.machine.touch(ws, now);
                if pool
                    .machine
                    .sharing(ws, now)
                    .is_some_and(|s| s.position.is_some())
                {
                    return Ok(false);
                }
                let start = pool.machine.reserve(ws, now);
                pool.run(cx);
                cx.notify();
                Ok(start.map_err(anyhow::Error::msg)?.lease == LeaseStart::Granted)
            })?
        })
    }

    /// Ends the workspace's reservation now (or once its call in flight ends). Whether another
    /// workspace was waiting for the device.
    pub fn release_reservation(&mut self, ws: WorkspaceId, cx: &mut Context<Self>) -> bool {
        let waited = self.machine.release_reservation(ws, Instant::now());
        self.run(cx);
        cx.notify();
        waited
    }

    /// Activity of the workspace's agent: its reservation lasts [`super::RESERVATION_IDLE`]
    /// from now and its place in line is kept. Whether it holds or waits for a device.
    pub fn touch(&mut self, ws: WorkspaceId, cx: &mut Context<Self>) -> bool {
        let found = self.machine.touch(ws, Instant::now());
        self.run(cx);
        found
    }

    /// The user gives `key` to `ws` now (the holder's call in flight finishes first). Whether
    /// `ws` holds it at once; refused when `ws` uses another device.
    pub fn take_over(
        &mut self,
        key: &PoolKey,
        ws: WorkspaceId,
        cx: &mut Context<Self>,
    ) -> Result<bool, String> {
        let start = self.machine.take_over(key, ws, Instant::now());
        self.run(cx);
        cx.notify();
        Ok(start? == LeaseStart::Granted)
    }

    /// Who uses the device the workspace uses, and when its turn comes; `None` until the pool
    /// knows that device.
    pub fn sharing(&self, ws: WorkspaceId) -> Option<Sharing> {
        self.machine.sharing(ws, Instant::now())
    }

    /// `ws` installed its build on `key` (a run launched it): later grants to others say so.
    pub fn app_installed(&mut self, key: &PoolKey, ws: WorkspaceId) {
        self.machine.app_installed(key, ws);
        self.stale_told.remove(&ws);
    }

    /// Tells the agent of `ws` once that the app on `key` is another workspace's build.
    pub(super) fn tell_stale(
        &mut self,
        ws: WorkspaceId,
        key: &PoolKey,
        by: Option<&str>,
        cx: &mut Context<Self>,
    ) {
        let Some(by) = by else {
            return;
        };
        let told = (key.clone(), by.to_owned());
        if self.stale_told.get(&ws) == Some(&told) {
            return;
        }
        self.stale_told.insert(ws, told);
        let device = self.machine.name_of(key).unwrap_or("the device").to_owned();
        cx.emit(PoolEvent::AgentNotice {
            ws,
            message: stale_notice(&device, by),
        });
    }
}

/// Told to the holder when others have waited [`super::policy::MAX_HOLD`] for its device.
pub(super) fn warn_notice(device: &str, waiting: &[String], ends_in: Duration) -> String {
    let who = match waiting {
        [] => "Another workspace is".to_owned(),
        [one] => format!("{one} is"),
        many => format!("{} are", many.join(", ")),
    };
    format!(
        "{who} waiting for {device}; your reservation ends in {} s. Call device_release when \
         you are done with the device.",
        ends_in.as_secs()
    )
}

/// Told to a workspace that lost its reservation.
pub(super) fn revoked_notice(device: &str, next: Option<&str>) -> String {
    let next = match next {
        Some(next) => format!(" {next} has it now."),
        None => String::new(),
    };
    format!(
        "Your reservation on {device} was revoked because another workspace was waiting.{next} \
         Your next device call waits in line."
    )
}

/// Told to a workspace the user switched to `platform` (device panel).
pub(super) fn switched_notice(platform: Platform) -> String {
    let platform = platform.label();
    format!(
        "The user switched this workspace to {platform}: device calls now use its {platform} \
         device. Run build_and_run to install the app there."
    )
}

/// Told to a workspace granted a device where another workspace's build is installed.
pub(super) fn stale_notice(device: &str, by: &str) -> String {
    format!(
        "The app on {device} was installed by {by}, not by this workspace: run build_and_run \
         before testing your changes."
    )
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::{Platform, revoked_notice, stale_notice, switched_notice, warn_notice};

    #[test]
    fn notices_name_the_device_and_who_waits() {
        let s = |v: &[&str]| v.iter().map(|s| (*s).to_owned()).collect::<Vec<_>>();
        let warn = warn_notice("iPhone 16", &s(&["ws-c"]), Duration::from_secs(60));
        assert_eq!(
            warn,
            "ws-c is waiting for iPhone 16; your reservation ends in 60 s. Call device_release \
             when you are done with the device."
        );
        assert!(
            warn_notice("Pixel", &s(&["a", "b"]), Duration::from_secs(5))
                .starts_with("a, b are waiting for Pixel")
        );
        assert!(warn_notice("Pixel", &[], Duration::ZERO).starts_with("Another workspace is"));
        assert_eq!(
            revoked_notice("iPhone 16", Some("ws-c")),
            "Your reservation on iPhone 16 was revoked because another workspace was waiting. \
             ws-c has it now. Your next device call waits in line."
        );
        assert!(!revoked_notice("iPhone 16", None).contains("has it now"));
        assert!(stale_notice("iPhone 16", "ws-a").contains("installed by ws-a"));
        assert_eq!(
            switched_notice(Platform::Android),
            "The user switched this workspace to Android: device calls now use its Android \
             device. Run build_and_run to install the app there."
        );
    }
}
