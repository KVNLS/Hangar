//! Leases: who uses a device for a run or tool call, and who waits.

use std::time::Instant;

use hangar_core::WorkspaceId;
use hangar_core::device::Platform;

use super::policy::{Acquire, LEASE_TIMEOUT};
use super::{Command, PoolKey, PoolMachine, Session, State, Token};

/// Result of [`PoolMachine::request_lease`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LeaseStart {
    Granted,
    /// Answered later by [`Command::GrantLease`] or [`Command::FailLease`] with this token.
    Queued(Token),
}

impl<S: Session> PoolMachine<S> {
    /// Asks for the lease on `key` for `ws` (granted at once when free or already `ws`'s).
    pub fn request_lease(
        &mut self,
        key: &PoolKey,
        ws: WorkspaceId,
        now: Instant,
    ) -> Result<LeaseStart, String> {
        let token = self.token();
        let device = self
            .entry(key)
            .ok_or_else(|| "no device assigned to this workspace".to_owned())?;
        let name = device.name.clone();
        match device.leases.acquire(ws, now, token) {
            Acquire::Granted(_) => {
                self.touch(key);
                Ok(LeaseStart::Granted)
            }
            Acquire::Queued { position } => {
                tracing::info!(target: "device_pool", workspace = %ws, device = %name, position, "waiting for the device");
                Ok(LeaseStart::Queued(token))
            }
        }
    }

    /// `holder` dropped its lease on `key`; the next waiter gets it.
    pub fn release_lease(&mut self, key: &PoolKey, holder: WorkspaceId) {
        let Some(device) = self.devices.get_mut(key) else {
            return;
        };
        let granted = device.leases.release(&holder);
        let next = device.leases.holder().copied();
        if let Some(next) = next {
            for token in granted {
                self.push(Command::GrantLease {
                    token,
                    key: key.clone(),
                    holder: next,
                });
            }
            tracing::info!(target: "device_pool", workspace = %next, ?key, "device lease granted");
        }
        self.touch(key);
        self.enforce_limit();
    }

    /// Waiters queued longer than [`LEASE_TIMEOUT`] give up.
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
