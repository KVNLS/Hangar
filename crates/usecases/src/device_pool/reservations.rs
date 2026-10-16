//! Reservations: the holder of a device keeps it between its calls, so another workspace cannot
//! use it between a screenshot and a tap.
//!
//! A reservation starts with the first call (or [`PoolMachine::reserve`]) and ends when
//! - the holder has no call in flight for [`RESERVATION_IDLE`] after its last activity
//!   (acquire, release, [`PoolMachine::touch`]);
//! - the holder releases it ([`PoolMachine::release_reservation`]);
//! - it is revoked: kept [`MAX_HOLD`] after another workspace started waiting, the holder is warned
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

/// What a waiting ticket gets when its workspace releases the device.
const RELEASED: &str = "the device was released by this workspace (device_release) while this \
                        call waited for it";

impl<S: Session> PoolMachine<S> {
    /// Ends the workspace's reservations now (or, with a call in flight, when it ends) and takes
    /// it out of line: its waiting tickets fail. Whether another workspace was waiting for a
    /// device it held.
    pub fn release_reservation(&mut self, ws: WorkspaceId, now: Instant) -> bool {
        let waited = self.leave(ws, now, RELEASED);
        self.prune_bindings();
        self.enforce_limit();
        waited
    }

    /// Fails the waiting tickets of `ws` with `message` and ends its reservations (once its
    /// calls in flight end). Whether another workspace was waiting for a device it held.
    pub(super) fn leave(&mut self, ws: WorkspaceId, now: Instant, message: &str) -> bool {
        let mut cancelled = Vec::new();
        for device in self.devices.values_mut() {
            cancelled.extend(device.leases.cancel(&ws));
        }
        for token in cancelled {
            self.push(Command::FailLease {
                token,
                message: message.into(),
            });
        }
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
    /// queue behind. Refused when `ws` uses another device: release that one first.
    pub fn take_over(
        &mut self,
        key: &PoolKey,
        ws: WorkspaceId,
        now: Instant,
    ) -> Result<LeaseStart, String> {
        if let Some(bound) = self.bindings.get(&ws).filter(|k| *k != key) {
            let name = self.name_of(bound).unwrap_or("another device");
            return Err(format!("this workspace uses {name}; release it first"));
        }
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
        self.bindings.insert(ws, key.clone());
        self.prune_bindings();
        self.mark_used(key);
        Ok(start)
    }

    /// Ends the reservations that expired and warns or revokes those kept too long
    /// ([`super::MONITOR_TICK`]).
    pub(super) fn expire_reservations(&mut self, now: Instant) {
        let keys: Vec<PoolKey> = self.devices.keys().cloned().collect();
        for key in keys {
            self.review(&key, now);
        }
        self.prune_bindings();
    }

    /// Applies the reservation rules to the holder of `key`.
    pub(super) fn review(&mut self, key: &PoolKey, now: Instant) {
        let Some(device) = self.devices.get_mut(key) else {
            return;
        };
        let waiting = device.leases.waiting_len() > 0;
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
                tracing::info!(target: "device_pool", workspace = %who, ?key, "others waited too long: warned");
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
