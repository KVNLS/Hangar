//! Reservations: the holder of a device keeps it between its calls, so another workspace cannot
//! use it between a screenshot and a tap.
//!
//! A reservation starts with the first call (or [`PoolMachine::reserve`]) and ends when
//! - the holder has no call in flight for [`RESERVATION_IDLE`] after its last activity
//!   (acquire, release, [`PoolMachine::touch`]);
//! - the holder releases it ([`PoolMachine::release_reservation`]);
//! - it is revoked: held for [`MAX_HOLD`] while others wait, the holder is warned
//!   ([`Command::WarnHolder`]) and loses it [`REVOKE_GRACE`] later, never during a call; or the
//!   user takes the device over ([`PoolMachine::take_over`]).
//!
//! Until then the device counts as busy, so it is not shut down to make room; once the
//! reservation expired with nobody waiting it is idle like any other device.

use std::time::Instant;

use hangar_core::WorkspaceId;

use super::leases::LeaseStart;
use super::policy::{self, Acquire, Due};
#[cfg(doc)]
use super::policy::{MAX_HOLD, RESERVATION_IDLE, REVOKE_GRACE};
use super::{Command, PoolKey, PoolMachine, Session};

impl<S: Session> PoolMachine<S> {
    /// Reserves the workspace's device without a call: queued like a call, but nothing is
    /// released afterwards; the reservation lasts until it expires, is released or revoked.
    pub fn reserve(&mut self, ws: WorkspaceId, now: Instant) -> Result<LeaseStart, String> {
        let key = self
            .key_for(ws)
            .ok_or_else(|| "no device assigned to this workspace".to_owned())?;
        self.queue_for(&key, ws, now, false)
    }

    /// Ends the workspace's reservations now (or, with a call in flight, when it ends). Whether
    /// anyone was waiting for the device.
    pub fn release_reservation(&mut self, ws: WorkspaceId, now: Instant) -> bool {
        let held: Vec<PoolKey> = self
            .devices
            .iter()
            .filter(|(_, d)| d.leases.holder() == Some(&ws))
            .map(|(k, _)| k.clone())
            .collect();
        let mut waited = false;
        for key in held {
            let Some(device) = self.devices.get_mut(&key) else {
                continue;
            };
            waited |= device.leases.waiting_len() > 0;
            let granted = device.leases.end(&ws, now);
            tracing::info!(target: "device_pool", workspace = %ws, ?key, "reservation released");
            self.push_grants(&key, granted);
        }
        self.enforce_limit();
        waited
    }

    /// Activity of the workspace: its reservation lasts [`RESERVATION_IDLE`] from now, and its
    /// waiting tickets [`super::LEASE_TIMEOUT`]. Whether it holds or waits for a device.
    pub fn touch(&mut self, ws: WorkspaceId, now: Instant) -> bool {
        let mut found = false;
        for device in self.devices.values_mut() {
            found |= device.leases.touch(&ws, now);
        }
        found
    }

    /// The user gives `key` to `ws` now: `ws` goes first in line (with a reservation) and the
    /// holder's reservation is revoked without waiting for [`MAX_HOLD`]. A call of the old holder
    /// in flight is not cut: `ws` gets the device when it ends, and the old holder's next calls
    /// queue behind.
    pub fn take_over(
        &mut self,
        key: &PoolKey,
        ws: WorkspaceId,
        now: Instant,
    ) -> Result<LeaseStart, String> {
        let token = self.token();
        let device = self
            .entry(key)
            .ok_or_else(|| "no such device in the pool".to_owned())?;
        let previous = device.leases.holder().copied().filter(|h| *h != ws);
        let mut start = match device.leases.jump(ws, now, token, false) {
            Acquire::Granted(_) => LeaseStart::Granted,
            Acquire::Queued { .. } => LeaseStart::Queued(token),
        };
        if let Some(previous) = previous {
            let mut granted = device.leases.end(&previous, now);
            tracing::info!(target: "device_pool", workspace = %ws, from = %previous, ?key, "device taken over");
            self.push(Command::Revoked {
                holder: previous,
                key: key.clone(),
            });
            if let Some(i) = granted.iter().position(|g| g.payload == token) {
                granted.remove(i);
                start = LeaseStart::Granted;
            }
            self.push_grants(key, granted);
        }
        self.mark_used(key);
        Ok(start)
    }

    /// Ends the reservations that expired and warns or revokes those held too long
    /// ([`super::MONITOR_TICK`]).
    pub(super) fn expire_reservations(&mut self, now: Instant) {
        let keys: Vec<PoolKey> = self.devices.keys().cloned().collect();
        for key in keys {
            self.review(&key, now);
        }
    }

    /// Applies the reservation rules to the holder of `key`.
    pub(super) fn review(&mut self, key: &PoolKey, now: Instant) {
        let Some(device) = self.devices.get_mut(key) else {
            return;
        };
        let waiting = device.leases.waiting_len() > 0;
        if !waiting {
            device.leases.clear_warning();
        }
        let Some(holder) = device.leases.holding() else {
            return;
        };
        let who = holder.who;
        match policy::reservation_due(holder, waiting, now) {
            Due::Keep => {}
            Due::Expire => {
                tracing::info!(target: "device_pool", workspace = %who, ?key, "reservation expired");
                let granted = device.leases.end(&who, now);
                self.push_grants(key, granted);
            }
            Due::Warn { ends_in } => {
                device.leases.warn(now);
                tracing::info!(target: "device_pool", workspace = %who, ?key, "held too long while others wait: warned");
                self.push(Command::WarnHolder {
                    holder: who,
                    key: key.clone(),
                    ends_in,
                });
            }
            Due::Revoke => {
                tracing::info!(target: "device_pool", workspace = %who, ?key, "reservation revoked");
                let granted = device.leases.end(&who, now);
                self.push(Command::Revoked {
                    holder: who,
                    key: key.clone(),
                });
                self.push_grants(key, granted);
            }
        }
    }
}
