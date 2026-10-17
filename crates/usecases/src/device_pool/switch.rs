//! Switching platform: a workspace has one platform attached at a time. Switching leaves the
//! device it uses (the next in line gets it), parks the active assignment and attaches the other
//! platform's; the parked own device is shut down but kept, so switching back reuses it.

use std::time::Instant;

use hangar_core::WorkspaceId;
use hangar_core::device::Platform;

use super::policy::Assignment;
use super::shutdown::Replaced;
use super::{Command, PoolKey, PoolMachine, Save, Session, ShutdownJob};

/// Result of [`PoolMachine::switch_platform`].
#[derive(Debug)]
pub enum Switched<S> {
    /// The target's platform is attached already: nothing changed. A new base goes through
    /// [`PoolMachine::assign`].
    Same,
    Done {
        /// The old platform's own device to shut down; it is kept for switching back. `None`
        /// when it does not run or another workspace uses it.
        shutdown: Option<ShutdownJob<S>>,
        /// The parked device of the target's platform was made from another base: shut it
        /// down, then delete its simulator.
        replaced: Option<Replaced<S>>,
    },
}

impl<S: Session> PoolMachine<S> {
    /// Attaches `target`, an assignment of the workspace's other platform (its parked one
    /// normally), in place of the active one, which is parked. Refused while a call or run of
    /// `ws` holds a device. The workspace leaves the device it uses: the next in line gets it,
    /// and its own waiting calls fail. Reserve a device afterwards ([`Self::reserve`]).
    pub fn switch_platform(
        &mut self,
        ws: WorkspaceId,
        mut target: Assignment,
        now: Instant,
    ) -> Result<Switched<S>, String> {
        let active = self.assignments.get(&ws).cloned();
        if active
            .as_ref()
            .is_some_and(|a| a.platform == target.platform)
        {
            return Ok(Switched::Same);
        }
        if let Some(name) = self.in_flight(ws) {
            return Err(format!(
                "a call or build still uses {name}; switch when it ends"
            ));
        }
        tracing::info!(target: "device_pool", workspace = %ws, platform = target.platform.label(), base = %target.base, "switching platform");
        let message = format!(
            "this workspace switched to {}; call again",
            target.platform.label()
        );
        self.unbind(ws, now, &message);
        let replaced = self.unpark(ws, &mut target);
        let mut shutdown = None;
        if let Some(active) = active {
            let old = PoolKey::Own(ws, active.platform);
            if !self.used_by_others(&old, ws) {
                shutdown = self.shutdown(&old);
            }
            self.parked.insert(ws, active.clone());
            self.save(Save::Parked(ws, active));
        }
        self.assignments.insert(ws, target.clone());
        self.save(Save::Assignment(ws, target));
        self.prune_bindings();
        self.enforce_limit();
        self.refocus();
        Ok(Switched::Done { shutdown, replaced })
    }

    /// Name of a device on which a call or run of `ws` is in flight.
    fn in_flight(&self, ws: WorkspaceId) -> Option<&str> {
        self.devices
            .values()
            .find(|d| {
                d.leases
                    .holding()
                    .is_some_and(|h| h.who == ws && h.holds > 0)
            })
            .map(|d| d.name.as_str())
    }

    /// Whether a workspace other than `ws` holds or waits for `key`.
    fn used_by_others(&self, key: &PoolKey, ws: WorkspaceId) -> bool {
        self.devices.get(key).is_some_and(|d| {
            d.leases.holder().is_some_and(|h| *h != ws)
                || d.leases.waiters().into_iter().any(|w| *w != ws)
        })
    }

    /// Takes the parked assignment of `ws` back when `target` is of its platform. Of the same
    /// base, `target` gets its own device back (simulator, serial); of another, that device is
    /// dropped ([`Self::drop_own`]).
    pub(super) fn unpark(
        &mut self,
        ws: WorkspaceId,
        target: &mut Assignment,
    ) -> Option<Replaced<S>> {
        if self
            .parked(ws)
            .is_none_or(|p| p.platform != target.platform)
        {
            return None;
        }
        let parked = self.parked.remove(&ws)?;
        if parked.base == target.base {
            target.own = target.own.take().or(parked.own);
            target.serial = target.serial.take().or(parked.serial);
            return None;
        }
        Some(self.drop_own(ws, &parked))
    }

    /// The own device of assignment `a` is replaced: it is shut down and leaves the pool, and
    /// the simulator the pool created for it is returned for deletion.
    pub(super) fn drop_own(&mut self, ws: WorkspaceId, a: &Assignment) -> Replaced<S> {
        let key = PoolKey::Own(ws, a.platform);
        let shutdown = self.shutdown(&key);
        self.remove_device(&key);
        Replaced {
            shutdown,
            clone: (a.platform == Platform::Ios)
                .then(|| a.own.clone())
                .flatten(),
        }
    }

    /// Two own devices replaced at once (a new platform assigned over a parked one of another
    /// base): one per platform, so at most one has a simulator to delete. It is returned; the
    /// other is shut down on its own.
    pub(super) fn merge_replaced(&mut self, a: Replaced<S>, b: Replaced<S>) -> Replaced<S> {
        let (keep, other) = if a.clone.is_some() { (a, b) } else { (b, a) };
        if let Some(job) = other.shutdown {
            self.push(Command::Shutdown(job));
        }
        keep
    }
}
