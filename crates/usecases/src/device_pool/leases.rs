//! Leases: who uses a device for a run or tool call, and who waits.

use std::time::Instant;

use hangar_core::WorkspaceId;

use super::policy::{Acquire, Grant, LEASE_TIMEOUT};
use super::{Command, PoolKey, PoolMachine, Session, State, Token};

/// Result of joining a device's lease queue.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LeaseStart {
    Granted,
    /// Answered later by [`Command::GrantLease`] or [`Command::FailLease`] with this token.
    Queued(Token),
}

impl<S: Session> PoolMachine<S> {
    /// Asks for the lease on `key` for a call of `ws` (granted at once when free or already
    /// `ws`'s). Release it with [`Self::release_lease`] when the call ends.
    #[cfg(test)]
    pub fn request_lease(
        &mut self,
        key: &PoolKey,
        ws: WorkspaceId,
        now: Instant,
    ) -> Result<LeaseStart, String> {
        self.queue_for(key, ws, now, true)
    }

    /// Joins the lease queue of `key`: a call (`hold`) or a bare reservation. `ws` is bound to
    /// `key` from now on.
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
        let acquired = device.leases.acquire(ws, now, token, hold);
        self.bindings.insert(ws, key.clone());
        match acquired {
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
        self.prune_bindings();
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
        let stale_app = self.stale_app(key, holder);
        for grant in granted {
            self.push(Command::GrantLease {
                token: grant.payload,
                key: key.clone(),
                holder,
                hold: grant.hold,
                stale_app: stale_app.clone(),
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
}

#[cfg(test)]
mod tests {
    use crate::device_pool::fixtures::*;
    use crate::device_pool::*;

    #[test]
    fn leases_queue_hand_over_and_time_out() {
        let (a, b, c) = (workspace("a"), workspace("b"), workspace("c"));
        let assigned = [(&a, ios("X")), (&b, ios("X")), (&c, ios("X"))];
        let mut m = pool(Limit::Manual(1), &assigned);
        let now = Instant::now();
        let x = base_key("X");
        assert_eq!(m.request_lease(&x, a.id, now), Ok(LeaseStart::Granted));
        let Ok(LeaseStart::Queued(for_b)) = m.request_lease(&x, b.id, now) else {
            panic!("b should wait");
        };
        let Ok(LeaseStart::Queued(for_c)) = m.request_lease(&x, c.id, now) else {
            panic!("c should wait");
        };
        assert_eq!(m.held_by_other(b.id).map(|(ws, _)| ws), Some(a.id));
        m.release_lease(&x, a.id, now);
        let commands = m.take_commands();
        assert!(matches!(
            commands.as_slice(),
            [Command::GrantLease { token, holder, .. }] if *token == for_b && *holder == b.id
        ));
        m.monitor(now + LEASE_TIMEOUT, Ok(Pressure::Normal), None);
        let commands = m.take_commands();
        assert!(matches!(
            commands.as_slice(),
            [Command::FailLease { token, message }]
                if *token == for_c && message == "waited 10 minutes for X without getting it"
        ));
        assert!(!m.is_waiting(c.id));
    }

    #[test]
    fn a_polled_ticket_survives_the_lease_timeout() {
        let (mut m, [a, b, c], x, t) = shared_base();
        m.request_lease(&x, a.id, t).unwrap();
        let for_b = queued(m.request_lease(&x, b.id, t));
        let for_c = queued(m.request_lease(&x, c.id, t));
        assert!(m.touch(b.id, t + secs(9 * 60)));
        assert_eq!(failed(&tick(&mut m, t + LEASE_TIMEOUT)), [for_c]);
        assert!(m.is_waiting(b.id));
        assert!(failed(&tick(&mut m, t + secs(9 * 60) + LEASE_TIMEOUT - secs(1))).is_empty());
        assert_eq!(
            failed(&tick(&mut m, t + secs(9 * 60) + LEASE_TIMEOUT)),
            [for_b]
        );
        assert!(!m.is_waiting(b.id));
    }
}
