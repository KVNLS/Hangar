//! Binding: which device a workspace uses right now. Its own device when a slot is free (or
//! single-device mode's base device); at the cap, a running device of the same class (platform
//! and base device) that another workspace uses, whose lease it then waits for, rather than
//! waiting for a slot of its own.
//!
//! The binding lasts while the workspace holds (a call or a reservation) or waits for the
//! device's lease, and is separate from the saved [`super::policy::Assignment`].

use std::time::Instant;

use hangar_core::WorkspaceId;
use hangar_core::device::Platform;

use super::leases::LeaseStart;
use super::policy::{self, Acquire, Capacity, Grant, Slot};
use super::{
    Command, NoticeLevel, OpenError, Phase, PoolDevice, PoolKey, PoolMachine, STOPPED, Session,
    State,
};

/// Devices that run (open, opening once booted, or left running).
const RUNNING: u8 = 2;
/// Also devices about to boot for a call.
const STARTING: u8 = 3;

/// Result of [`PoolMachine::request_device`]: the device to open (with
/// [`PoolMachine::open`]) once the lease is granted.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DeviceStart {
    /// For a queued lease the grant names the device again ([`Command::GrantLease`]), which
    /// wins: a waiting ticket may move to another device meanwhile.
    pub key: PoolKey,
    pub lease: LeaseStart,
    /// Not the workspace's own device: another workspace uses it too.
    pub shared: bool,
    /// Granted at once on a device where another workspace installed its build last.
    pub stale_app: Option<String>,
}

impl<S: Session> PoolMachine<S> {
    /// A device for a call of `ws`, with its lease (release it with
    /// [`Self::release_lease`] when the call ends):
    /// 1. the device `ws` already uses (re-entrant);
    /// 2. in single-device mode, its platform's base device;
    /// 3. its own device when it runs or a slot is free (or can be freed);
    /// 4. at the cap, the compatible running device with the shortest queue, shared;
    /// 5. else its own device, which waits for a slot when opened.
    pub fn request_device(&mut self, ws: WorkspaceId, now: Instant) -> Result<DeviceStart, String> {
        self.start_device(ws, now, true)
    }

    /// Like [`Self::request_device`], but a reservation without a call: nothing to release; it
    /// lasts until it expires, is released or revoked.
    pub fn reserve(&mut self, ws: WorkspaceId, now: Instant) -> Result<DeviceStart, String> {
        self.start_device(ws, now, false)
    }

    fn start_device(
        &mut self,
        ws: WorkspaceId,
        now: Instant,
        hold: bool,
    ) -> Result<DeviceStart, String> {
        let bound = self.bindings.get(&ws).cloned();
        let key = match &bound {
            Some(key) => key.clone(),
            None => self.choose_device(ws)?,
        };
        let lease = self.queue_for(&key, ws, now, hold)?;
        let shared = !self.owns(ws, &key);
        if shared && bound.is_none() {
            self.notice_shared(ws, &key, None);
        }
        Ok(DeviceStart {
            stale_app: (lease == LeaseStart::Granted)
                .then(|| self.stale_app(&key, ws))
                .flatten(),
            key,
            lease,
            shared,
        })
    }

    fn choose_device(&self, ws: WorkspaceId) -> Result<PoolKey, String> {
        let a = self
            .assignments
            .get(&ws)
            .ok_or_else(|| "no device assigned to this workspace".to_owned())?;
        let own = self.key_of(ws, a);
        if self.single_device(a.platform) {
            return Ok(own);
        }
        let started = self.devices.get(&own).is_some_and(|d| {
            matches!(d.state, State::Opening(_) | State::Open { .. })
                || (d.running && matches!(d.state, State::Closed))
        });
        if started {
            return Ok(own);
        }
        let slots = self.committed_slots(&own);
        match policy::capacity(&slots, self.limit_for(a.platform)) {
            Capacity::Free | Capacity::Evict(_) => Ok(own),
            Capacity::Full => Ok(self
                .shareable(a.platform, &a.base, &own, STARTING)
                .unwrap_or(own)),
        }
    }

    /// Like [`Self::slots`], with devices about to boot for a call taking a slot already, so
    /// that calls arriving together do not all count on the same free slot.
    fn committed_slots(&self, except: &PoolKey) -> Vec<Slot<PoolKey>> {
        let mut slots = self.slots(Some(except));
        for slot in &mut slots {
            if let Some(d) = self.devices.get(&slot.key)
                && !matches!(d.state, State::ShuttingDown)
                && (matches!(d.state, State::Opening(_)) || in_use(d))
            {
                slot.booted = true;
            }
        }
        slots
    }

    /// Whether `key` is `ws`'s own device (or the base device it is assigned in single-device
    /// mode).
    fn owns(&self, ws: WorkspaceId, key: &PoolKey) -> bool {
        self.assignments
            .get(&ws)
            .is_some_and(|a| self.key_of(ws, a) == *key)
    }

    /// The base device `key` is made from (its class, with its platform).
    fn class_base<'a>(&'a self, key: &'a PoolKey) -> Option<&'a str> {
        match key {
            PoolKey::Base(k) => Some(k.id.as_str()),
            PoolKey::Own(ws) => self.assignments.get(ws).map(|a| a.base.as_str()),
        }
    }

    /// A running device of the class of `key` (same platform and base) other than `key`, the
    /// one with the shortest queue. Open ones first, then ones opening or left running (which
    /// need no boot).
    pub fn borrowable(&self, platform: Platform, base: &str, key: &PoolKey) -> Option<PoolKey> {
        self.shareable(platform, base, key, RUNNING)
    }

    fn shareable(
        &self,
        platform: Platform,
        base: &str,
        except: &PoolKey,
        max_rank: u8,
    ) -> Option<PoolKey> {
        self.devices
            .iter()
            .filter(|(k, d)| {
                *k != except && d.platform == platform && self.class_base(k) == Some(base)
            })
            .filter_map(|(k, d)| {
                let rank = share_rank(d).filter(|r| *r <= max_rank)?;
                let load = d.leases.waiters().len() + usize::from(d.leases.holder().is_some());
                Some(((rank, load, d.name.as_str()), k))
            })
            .min_by(|(a, _), (b, _)| a.cmp(b))
            .map(|(_, k)| k.clone())
    }

    /// The boot of `key` was refused ([`OpenError::Refused`]): the call of `ws` moves to a
    /// compatible running device, and so do the others waiting for `key`. `None` when none
    /// runs; the call has no device then. Drop the lease on `key` before or after: it is no
    /// longer `ws`'s.
    pub fn boot_refused(
        &mut self,
        ws: WorkspaceId,
        key: &PoolKey,
        now: Instant,
    ) -> Result<Option<DeviceStart>, String> {
        if let Some(bound) = self.bindings.get(&ws).filter(|k| *k != key).cloned() {
            // An earlier refused call of `ws` moved it already.
            let lease = self.queue_for(&bound, ws, now, true)?;
            return Ok(Some(DeviceStart {
                stale_app: (lease == LeaseStart::Granted)
                    .then(|| self.stale_app(&bound, ws))
                    .flatten(),
                shared: !self.owns(ws, &bound),
                key: bound,
                lease,
            }));
        }
        let device = self
            .devices
            .get(key)
            .ok_or_else(|| "no such device in the pool".to_owned())?;
        let platform = device.platform;
        let waiters: Vec<WorkspaceId> = device.leases.waiters().into_iter().copied().collect();
        let to = self
            .class_base(key)
            .and_then(|base| self.shareable(platform, base, key, RUNNING));
        self.drop_holder(key, ws, now);
        let Some(to) = to else {
            self.prune_bindings();
            return Ok(None);
        };
        for other in waiters {
            self.move_tickets(other, key, &to, now);
        }
        let lease = self.queue_for(&to, ws, now, true)?;
        self.notice_shared(ws, &to, Some("could not boot its own device"));
        self.prune_bindings();
        Ok(Some(DeviceStart {
            stale_app: (lease == LeaseStart::Granted)
                .then(|| self.stale_app(&to, ws))
                .flatten(),
            key: to,
            lease,
            shared: true,
        }))
    }

    /// At the cap, while `key` waits for a slot: the workspaces waiting for its lease move to a
    /// compatible device that runs, if one does, and so does the holder whose call waits for
    /// the boot: the bring-up stops and its waiters get [`OpenError::Moved`]. Whether it
    /// stopped.
    pub(super) fn reroute(&mut self, key: &PoolKey, now: Instant) -> bool {
        let Some(device) = self.devices.get(key) else {
            return false;
        };
        let platform = device.platform;
        let holder = device
            .leases
            .holding()
            .filter(|h| h.holds > 0)
            .map(|h| h.who);
        let waiters: Vec<WorkspaceId> = device.leases.waiters().into_iter().copied().collect();
        if holder.is_none() && waiters.is_empty() {
            return false;
        }
        let Some(to) = self
            .class_base(key)
            .and_then(|base| self.shareable(platform, base, key, RUNNING))
        else {
            return false;
        };
        for ws in waiters {
            self.move_tickets(ws, key, &to, now);
        }
        let Some(holder) = holder else {
            self.prune_bindings();
            return false;
        };
        tracing::info!(target: "device_pool", workspace = %holder, from = ?key, ?to, "every slot is busy: moving to a running device");
        self.drop_holder(key, holder, now);
        self.waiting_slot.remove(key);
        let to_name = self.name_of(&to).unwrap_or("a running device").to_owned();
        if let Some(device) = self.devices.get_mut(key) {
            let state = std::mem::replace(&mut device.state, State::Closed);
            if let State::Opening(opening) = state {
                let message = format!(
                    "every device slot is in use: request the device again to share {to_name}"
                );
                for token in opening.waiters {
                    self.push(Command::ReplyOpen {
                        token,
                        result: Err(OpenError::Moved {
                            to: to.clone(),
                            message: message.clone(),
                        }),
                    });
                }
            }
        }
        self.prune_bindings();
        true
    }

    /// Takes the lease on `key` from `ws` whatever it has in flight.
    pub(super) fn drop_holder(&mut self, key: &PoolKey, ws: WorkspaceId, now: Instant) {
        if let Some(device) = self.devices.get_mut(key) {
            let granted = device.leases.drop_holder(&ws, now);
            self.push_grants(key, granted);
        }
    }

    /// Moves the waiting tickets of `ws` from `from` to `to`, granted there at once if free.
    fn move_tickets(&mut self, ws: WorkspaceId, from: &PoolKey, to: &PoolKey, now: Instant) {
        let Some(tickets) = self.devices.get_mut(from).map(|d| d.leases.take(&ws)) else {
            return;
        };
        let Some(device) = self.entry(to) else {
            for (token, _) in tickets {
                self.push(Command::FailLease {
                    token,
                    message: STOPPED.into(),
                });
            }
            return;
        };
        let mut granted = Vec::new();
        for (token, hold) in tickets {
            if let Acquire::Granted(payload) = device.leases.acquire(ws, now, token, hold) {
                granted.push(Grant { payload, hold });
            }
        }
        self.bindings.insert(ws, to.clone());
        self.push_grants(to, granted);
        tracing::info!(target: "device_pool", workspace = %ws, ?from, ?to, "waiting for a running device instead");
        self.notice_shared(ws, to, None);
    }

    /// Tells the user `ws` now shares `key`, and with whom.
    fn notice_shared(&mut self, ws: WorkspaceId, key: &PoolKey, why: Option<&str>) {
        let name = self.workspace_name(ws).unwrap_or("A workspace").to_owned();
        let device = self.name_of(key).unwrap_or("a running device").to_owned();
        let mut with = self.bound_names(key, ws);
        if with.is_empty()
            && let PoolKey::Own(owner) = key
            && let Some(owner) = self.workspace_name(*owner)
        {
            with.push(owner.to_owned());
        }
        let with = if with.is_empty() {
            String::new()
        } else {
            format!(" with {}", with.join(", "))
        };
        let message = match why {
            Some(why) => format!("{name} {why}: it shares {device}{with} and takes turns."),
            None => {
                let limit = self
                    .devices
                    .get(key)
                    .map_or(1, |d| self.limit_for(d.platform));
                format!(
                    "All {limit} device slots are in use: {name} shares {device}{with} and takes \
                     turns."
                )
            }
        };
        self.notice(NoticeLevel::Info, message);
    }

    /// Names of the workspaces bound to `key` other than `except`, sorted.
    pub(super) fn bound_names(&self, key: &PoolKey, except: WorkspaceId) -> Vec<String> {
        let mut names: Vec<String> = self
            .bindings
            .iter()
            .filter(|(ws, k)| *k == key && **ws != except)
            .map(|(ws, _)| {
                self.workspace_name(*ws)
                    .unwrap_or("another workspace")
                    .to_owned()
            })
            .collect();
        names.sort();
        names
    }

    /// Drops the bindings of workspaces that neither hold nor wait for their device any more.
    pub(super) fn prune_bindings(&mut self) {
        let devices = &self.devices;
        self.bindings.retain(|ws, key| {
            devices
                .get(key)
                .is_some_and(|d| d.leases.holder() == Some(ws) || d.leases.is_waiting(ws))
        });
    }

    /// `ws` installed its build on `key`: a later grant to another workspace says so.
    pub fn app_installed(&mut self, key: &PoolKey, ws: WorkspaceId) {
        if let Some(device) = self.devices.get_mut(key) {
            device.installed_by = Some(ws);
        }
    }

    /// The workspace whose build is on `key`, when it is not `ws`.
    pub(super) fn stale_app(&self, key: &PoolKey, ws: WorkspaceId) -> Option<String> {
        let by = self.devices.get(key)?.installed_by.filter(|by| *by != ws)?;
        Some(
            self.workspace_name(by)
                .unwrap_or("a closed workspace")
                .to_owned(),
        )
    }
}

/// A call holds the lease or waits for it.
fn in_use<S>(d: &PoolDevice<S>) -> bool {
    d.leases.holding().is_some_and(|h| h.holds > 0) || d.leases.waiting_len() > 0
}

/// How good a device is to share: open, opening once booted, left running, then about to boot
/// for a call ([`STARTING`]). `None`: off, waiting for a slot, or stopping.
fn share_rank<S>(d: &PoolDevice<S>) -> Option<u8> {
    match &d.state {
        State::Open { .. } => Some(0),
        State::Opening(_) if d.running => Some(1),
        State::Closed if d.running => Some(2),
        State::Opening(o) if !matches!(o.phase, Phase::Claiming { .. }) => Some(STARTING),
        State::Closed if d.leases.holding().is_some_and(|h| h.holds > 0) => Some(STARTING),
        _ => None,
    }
}
