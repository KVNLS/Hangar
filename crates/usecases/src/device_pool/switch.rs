//! Switching platform: a workspace has one platform attached at a time. Switching leaves the
//! device it uses (the next in line gets it), parks the active assignment and attaches the other
//! platform's; the parked own device is shut down once nothing uses it, but kept, so switching
//! back reuses it.

use std::time::Instant;

use hangar_core::WorkspaceId;
use hangar_core::device::Platform;

use super::policy::Assignment;
use super::shutdown::Replaced;
use super::{Command, OpenError, Phase, PoolKey, PoolMachine, Save, Session, ShutdownJob, State};

/// Result of [`PoolMachine::switch_platform`].
#[derive(Debug)]
pub enum Switched<S> {
    /// The target's platform is attached already: nothing changed. A new base goes through
    /// [`PoolMachine::assign`].
    Same,
    Done {
        /// The old platform's own device to shut down; it is kept for switching back. `None`
        /// when it does not run or another workspace uses it (it is shut down once they leave).
        shutdown: Option<ShutdownJob<S>>,
        /// The parked device of the target's platform was made from another base: shut it
        /// down, then delete its simulator.
        replaced: Option<Replaced<S>>,
    },
}

/// Who asked for a platform switch.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SwitchBy {
    /// The workspace's agent (`device_acquire`), which gets the answer.
    Agent,
    /// The user (device panel): the agent is told ([`Command::UserSwitched`]).
    User,
}

impl<S: Session> PoolMachine<S> {
    /// Attaches `target`, an assignment of the workspace's other platform (its parked one
    /// normally), in place of the active one, which is parked. Decided now: [`Switched::Same`]
    /// when `target`'s platform is attached already. Refused while a call or run of `ws` holds a
    /// device, and while a call of another workspace uses the parked device `target` replaces.
    /// The workspace leaves the device it uses: the next in line gets it, and its own waiting
    /// calls fail. Reserve a device afterwards ([`Self::reserve`]).
    pub fn switch_platform(
        &mut self,
        ws: WorkspaceId,
        mut target: Assignment,
        by: SwitchBy,
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
        self.check_replaceable(ws, self.replaced_parked(ws, &target))?;
        tracing::info!(target: "device_pool", workspace = %ws, platform = target.platform.label(), base = %target.base, ?by, "switching platform");
        let platform = target.platform;
        let message = format!(
            "this workspace switched to {}; call again",
            platform.label()
        );
        self.unbind(ws, now, &message);
        let unparking = self.parked(ws).is_some_and(|p| p.platform == platform);
        let replaced = self.unpark(ws, &mut target, now);
        let mut shutdown = None;
        if let Some(active) = active {
            let old = PoolKey::Own(ws, active.platform);
            if !self.used_by_others(&old, ws) {
                shutdown = self.shutdown(&old);
            }
            self.parked.insert(ws, active.clone());
            self.save(Save::Parked(ws, active));
        } else if unparking {
            // With an active one, the parked save above replaces it.
            self.save(Save::Unparked(ws));
        }
        self.assignments.insert(ws, target.clone());
        self.save(Save::Assignment(ws, target));
        if by == SwitchBy::User {
            self.push(Command::UserSwitched { ws, platform });
        }
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

    /// The parked device of `ws` that attaching `target` drops: of `target`'s platform, made
    /// from another base.
    pub(super) fn replaced_parked(&self, ws: WorkspaceId, target: &Assignment) -> Option<PoolKey> {
        self.parked(ws)
            .filter(|p| p.platform == target.platform && p.base != target.base)
            .map(|p| PoolKey::Own(ws, p.platform))
    }

    /// Refuses to drop `key`, an own device of `ws`, while a call of another workspace uses it.
    pub(super) fn check_replaceable(
        &self,
        ws: WorkspaceId,
        key: Option<PoolKey>,
    ) -> Result<(), String> {
        let Some(device) = key.as_ref().and_then(|k| self.devices.get(k)) else {
            return Ok(());
        };
        match device.leases.holding() {
            Some(h) if h.who != ws && h.holds > 0 => {
                let by = self.workspace_name(h.who).unwrap_or("another workspace");
                Err(format!(
                    "{} is in use by a call of {by}; change it when that call ends",
                    device.name
                ))
            }
            _ => Ok(()),
        }
    }

    /// Takes the parked assignment of `ws` back when `target` is of its platform. Of the same
    /// base, `target` gets its own device back (simulator, serial); of another, that device is
    /// dropped ([`Self::drop_own`]).
    pub(super) fn unpark(
        &mut self,
        ws: WorkspaceId,
        target: &mut Assignment,
        now: Instant,
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
        Some(self.drop_own(ws, &parked, now))
    }

    /// The own device of assignment `a` is replaced: the other workspaces using it lose it
    /// (waiters fail, a reservation is revoked), it is shut down and leaves the pool, and the
    /// simulator the pool created for it is returned for deletion. Check
    /// [`Self::check_replaceable`] first.
    pub(super) fn drop_own(
        &mut self,
        ws: WorkspaceId,
        a: &Assignment,
        now: Instant,
    ) -> Replaced<S> {
        let key = PoolKey::Own(ws, a.platform);
        if let Some(name) = self.name_of(&key) {
            let message = format!("{name} was replaced: its workspace chose another base device");
            self.evict_users(&key, ws, &message, now);
        }
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

    /// A parked device no workspace holds or waits for.
    fn parked_unused(&self, key: &PoolKey) -> bool {
        let PoolKey::Own(ws, platform) = key else {
            return false;
        };
        self.parked(*ws).is_some_and(|a| a.platform == *platform)
            && self.devices.get(key).is_some_and(|d| d.leases.is_idle())
    }

    /// Shuts down the parked devices nobody uses (kept for switching back), except those of
    /// `except`, and stops their bring-ups that have not booted anything yet; one that boots or
    /// opens is shut down once it is open.
    pub(super) fn retire_parked(&mut self, except: Option<WorkspaceId>) {
        let unused: Vec<PoolKey> = self
            .devices
            .keys()
            .filter(|k| !matches!(k, PoolKey::Own(ws, _) if Some(*ws) == except))
            .filter(|k| self.parked_unused(k))
            .cloned()
            .collect();
        for key in unused {
            let Some(device) = self.devices.get(&key) else {
                continue;
            };
            match &device.state {
                State::Open { .. } => {}
                State::Closed if device.running => {}
                State::Opening(o)
                    if matches!(
                        o.phase,
                        Phase::Probing | Phase::Claiming { .. } | Phase::CheckingHost { .. }
                    ) =>
                {
                    self.abandon_parked(&key);
                    continue;
                }
                _ => continue,
            }
            tracing::info!(target: "device_pool", ?key, name = %device.name, "parked device unused: shutting down");
            if let Some(job) = self.shutdown(&key) {
                self.push(Command::Shutdown(job));
            }
        }
    }

    /// Stops the bring-up of a parked device nobody uses, between two of its steps (before it
    /// boots); its open waiters fail. Whether it stopped.
    pub(super) fn abandon_parked(&mut self, key: &PoolKey) -> bool {
        let PoolKey::Own(ws, _) = key else {
            return false;
        };
        if !self.parked_unused(key) {
            return false;
        }
        let message = match self.assignment(*ws) {
            Some(a) => format!("this workspace switched to {}", a.platform.label()),
            None => "this workspace switched platform".to_owned(),
        };
        let Some(device) = self
            .devices
            .get_mut(key)
            .filter(|d| matches!(d.state, State::Opening(_)))
        else {
            return false;
        };
        let State::Opening(opening) = std::mem::replace(&mut device.state, State::Closed) else {
            return false;
        };
        if opening.phase != Phase::Probing {
            // Only a slot was claimed.
            device.running = false;
        }
        let running = device.running;
        tracing::info!(target: "device_pool", ?key, "parked device unused: not starting it");
        self.waiting_slot.remove(key);
        for token in opening.waiters {
            self.push(Command::ReplyOpen {
                token,
                result: Err(OpenError::Failed(message.clone())),
            });
        }
        if running && let Some(job) = self.shutdown(key) {
            self.push(Command::Shutdown(job));
        }
        true
    }
}
