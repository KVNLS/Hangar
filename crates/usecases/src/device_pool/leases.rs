//! Leases: who uses a device for a run or tool call, and who waits.

use std::time::Instant;

use hangar_core::WorkspaceId;
use hangar_core::device::Platform;

use super::policy::{Acquire, Grant, LEASE_TIMEOUT};
use super::{Command, PoolKey, PoolMachine, Session, State, Token};

/// Result of [`PoolMachine::request_lease`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LeaseStart {
    Granted,
    /// Answered later by [`Command::GrantLease`] or [`Command::FailLease`] with this token.
    Queued(Token),
}

impl<S: Session> PoolMachine<S> {
    /// Asks for the lease on `key` for a call of `ws` (granted at once when free or already
    /// `ws`'s). Release it with [`Self::release_lease`] when the call ends.
    pub fn request_lease(
        &mut self,
        key: &PoolKey,
        ws: WorkspaceId,
        now: Instant,
    ) -> Result<LeaseStart, String> {
        self.queue_for(key, ws, now, true)
    }

    /// Joins the lease queue of `key`: a call (`hold`) or a bare reservation.
    pub(super) fn queue_for(
        &mut self,
        key: &PoolKey,
        ws: WorkspaceId,
        now: Instant,
        hold: bool,
    ) -> Result<LeaseStart, String> {
        let token = self.token();
        let device = self
            .entry(key)
            .ok_or_else(|| "no device assigned to this workspace".to_owned())?;
        let name = device.name.clone();
        match device.leases.acquire(ws, now, token, hold) {
            Acquire::Granted(_) => {
                self.mark_used(key);
                Ok(LeaseStart::Granted)
            }
            Acquire::Queued { position } => {
                tracing::info!(target: "device_pool", workspace = %ws, device = %name, position, "waiting for the device");
                Ok(LeaseStart::Queued(token))
            }
        }
    }

    /// A call of `holder` on `key` ended. The holder keeps the device as an idle reservation
    /// ([`super::RESERVATION_IDLE`]), except when the device is off: there is no state to keep
    /// between calls, so the next waiter gets it at once.
    pub fn release_lease(&mut self, key: &PoolKey, holder: WorkspaceId, now: Instant) {
        let Some(device) = self.devices.get_mut(key) else {
            return;
        };
        let mut granted = device.leases.release(&holder, now);
        let off = matches!(device.state, State::Closed) && !device.running;
        if off
            && device
                .leases
                .holding()
                .is_some_and(|h| h.who == holder && h.holds == 0)
        {
            granted.extend(device.leases.end(&holder, now));
        }
        self.push_grants(key, granted);
        self.review(key, now);
        self.mark_used(key);
        self.enforce_limit();
    }

    /// Answers the waiters granted the lease on `key`.
    pub(super) fn push_grants(&mut self, key: &PoolKey, granted: Vec<Grant<Token>>) {
        if granted.is_empty() {
            return;
        }
        let Some(holder) = self
            .devices
            .get(key)
            .and_then(|d| d.leases.holder())
            .copied()
        else {
            return;
        };
        for grant in granted {
            self.push(Command::GrantLease {
                token: grant.payload,
                key: key.clone(),
                holder,
                hold: grant.hold,
                stale_app: None,
            });
        }
        tracing::info!(target: "device_pool", workspace = %holder, ?key, "device lease granted");
    }

    /// Waiters not seen (queued or polled) for [`LEASE_TIMEOUT`] give up.
    pub(super) fn expire_leases(&mut self, now: Instant) {
        let mut expired = Vec::new();
        for device in self.devices.values_mut() {
            for (ws, token) in device.leases.expire(now, LEASE_TIMEOUT) {
                tracing::warn!(target: "device_pool", workspace = %ws, device = %device.name, "gave up waiting for the device");
                expired.push((
                    token,
                    format!(
                        "waited {} minutes for {} without getting it",
                        LEASE_TIMEOUT.as_secs() / 60,
                        device.name
                    ),
                ));
            }
        }
        for (token, message) in expired {
            self.push(Command::FailLease { token, message });
        }
    }

    /// Whether the workspace waits for a device (lease queue or a free slot).
    pub fn is_waiting(&self, ws: WorkspaceId) -> bool {
        self.devices.values().any(|d| d.leases.is_waiting(&ws))
            || self
                .key_for(ws)
                .is_some_and(|k| self.waiting_slot.contains(&k))
    }

    /// A running device of `platform` other than `key`, the one with the shortest queue.
    /// Open ones first, then ones opening or left running (which need no boot).
    pub fn borrowable(&self, platform: Platform, key: &PoolKey) -> Option<PoolKey> {
        self.devices
            .iter()
            .filter(|(k, d)| *k != key && d.platform == platform)
            .filter_map(|(k, d)| {
                let rank = match d.state {
                    State::Open { .. } => 0,
                    State::Opening(_) => 1,
                    State::Closed if d.running => 2,
                    _ => return None,
                };
                Some(((rank, d.leases.waiting_len()), k))
            })
            .min_by_key(|(rank, _)| *rank)
            .map(|(_, k)| k.clone())
    }
}
