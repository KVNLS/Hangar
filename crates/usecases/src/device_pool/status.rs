//! What the panel, Settings and the status bar show about the pool.

use std::time::{Duration, Instant};

use hangar_core::WorkspaceId;
use hangar_core::device::{BackendReason, DeviceKey, Platform, Target, clone_name};

use super::policy::{self, MAX_HOLD, REVOKE_GRACE};
use super::{PoolKey, PoolMachine, Session, State};

/// What the device panel shows for a workspace.
#[derive(Debug)]
pub enum DeviceStatus<'a, S> {
    /// Nothing known yet (assignment not loaded).
    Unknown,
    Closed {
        name: &'a str,
        error: Option<&'a str>,
        /// An emulator outside the pool blocks the boot (its serial).
        conflict: Option<&'a str>,
    },
    Opening {
        name: &'a str,
        status: String,
    },
    Open {
        session: &'a S,
        reason: Option<&'a BackendReason>,
    },
    ShuttingDown {
        name: &'a str,
    },
}

/// A row of Settings › Devices.
#[derive(Clone, Debug, PartialEq)]
pub struct DeviceRow {
    pub key: PoolKey,
    pub platform: Platform,
    pub name: String,
    pub state: &'static str,
    pub busy: bool,
    /// The workspace holding its lease (a call or a reservation).
    pub holder: Option<String>,
    /// Workspaces waiting for its lease.
    pub queue_len: usize,
    /// Workspaces using it.
    pub workspaces: Vec<String>,
    /// A simulator the pool created (can be deleted).
    pub clone_udid: Option<String>,
}

/// A workspace, by id and name.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Member {
    pub id: WorkspaceId,
    pub name: String,
}

/// Who uses the device a workspace is bound to, and its turn ([`PoolMachine::sharing`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Sharing {
    pub key: PoolKey,
    pub device: String,
    /// adb serial or simulator UDID, when known.
    pub device_id: Option<String>,
    /// Holds the lease (a call or a reservation).
    pub holder: Option<Member>,
    /// Waiting for the lease, in turn order.
    pub queue: Vec<Member>,
    /// The workspace's place in `queue` (0 = next); `None` when it does not wait.
    pub position: Option<usize>,
    /// The other workspaces using the device now.
    pub shared_with: Vec<String>,
    /// Until the holder's reservation ends at the latest (idle expiry or revocation); `None`
    /// while a call is in flight and nobody waits.
    pub reservation_left: Option<Duration>,
    /// A rough wait for the workspace's turn: what the holder may still keep it (its
    /// reservation left, the hold limit counting from the first waiter), plus [`MAX_HOLD`] for
    /// each workspace ahead.
    pub estimated_wait: Option<Duration>,
    /// The workspace whose build was installed last, when not this one.
    pub stale_app_from: Option<String>,
}

impl<S: Session> PoolMachine<S> {
    /// Who uses the device `ws` uses (bound to, else its own) and when its turn comes; `None`
    /// until the pool knows that device.
    pub fn sharing(&self, ws: WorkspaceId, now: Instant) -> Option<Sharing> {
        let key = self.key_for(ws)?;
        let device = self.devices.get(&key)?;
        let queue: Vec<Member> = device
            .leases
            .waiters()
            .into_iter()
            .map(|w| self.member(*w))
            .collect();
        let position = queue.iter().position(|m| m.id == ws);
        let holding = device.leases.holding();
        let reservation_left =
            holding.and_then(|h| policy::reservation_left(h, !queue.is_empty(), now));
        let estimated_wait = position.map(|ahead| {
            let holder_left = match holding {
                Some(_) => reservation_left.unwrap_or(MAX_HOLD + REVOKE_GRACE),
                None => Duration::ZERO,
            };
            holder_left + MAX_HOLD * ahead as u32
        });
        Some(Sharing {
            device: device.name.clone(),
            device_id: device.device_id(),
            holder: holding.map(|h| self.member(h.who)),
            queue,
            position,
            shared_with: self.bound_names(&key, ws),
            reservation_left,
            estimated_wait,
            stale_app_from: self.stale_app(&key, ws),
            key,
        })
    }

    fn member(&self, ws: WorkspaceId) -> Member {
        Member {
            id: ws,
            name: self
                .workspace_name(ws)
                .unwrap_or("another workspace")
                .to_owned(),
        }
    }

    /// What the panel shows for the workspace at `now`.
    pub fn status(&self, ws: WorkspaceId, now: Instant) -> DeviceStatus<'_, S> {
        let Some(key) = self.key_for(ws) else {
            return DeviceStatus::Unknown;
        };
        let Some(device) = self.devices.get(&key) else {
            let name = match (&key, self.assignments.get(&ws)) {
                (PoolKey::Base(_), Some(a)) => a.base.as_str(),
                _ => "This workspace's device",
            };
            return DeviceStatus::Closed {
                name,
                error: None,
                conflict: None,
            };
        };
        match &device.state {
            State::Closed => DeviceStatus::Closed {
                name: &device.name,
                error: device.error.as_deref(),
                conflict: device.conflict.as_ref().map(|c| c.serial.as_str()),
            },
            State::Opening(o) => DeviceStatus::Opening {
                name: &device.name,
                status: format!(
                    "{}… {} s",
                    o.stage,
                    now.saturating_duration_since(o.started).as_secs()
                ),
            },
            State::Open { session, reason } => DeviceStatus::Open {
                session,
                reason: reason.as_ref(),
            },
            State::ShuttingDown => DeviceStatus::ShuttingDown { name: &device.name },
        }
    }

    /// Another workspace holding the lease on `ws`'s device (a run or tool call): its id and
    /// name.
    pub fn held_by_other(&self, ws: WorkspaceId) -> Option<(WorkspaceId, &str)> {
        let holder = *self.devices.get(&self.key_for(ws)?)?.leases.holder()?;
        (holder != ws).then(|| {
            let name = self
                .workspaces
                .get(&holder)
                .map_or("another workspace", |w| w.name.as_str());
            (holder, name)
        })
    }

    /// Whether the lease on the base device `key` is held by a workspace other than `ws`.
    pub fn base_held_by_other(&self, key: &DeviceKey, ws: WorkspaceId) -> bool {
        self.devices
            .get(&PoolKey::Base(key.clone()))
            .and_then(|d| d.leases.holder())
            .is_some_and(|holder| *holder != ws)
    }

    /// The open device of the workspace, if any (no boot).
    pub fn session(&self, ws: WorkspaceId) -> Option<&S> {
        match &self.devices.get(&self.key_for(ws)?)?.state {
            State::Open { session, .. } => Some(session),
            _ => None,
        }
    }

    /// Every device the pool knows, for Settings › Devices.
    pub fn rows(&self) -> Vec<DeviceRow> {
        let mut rows: Vec<DeviceRow> = self
            .devices
            .iter()
            .map(|(key, d)| {
                let workspaces = self
                    .assignments
                    .keys()
                    .filter(|ws| self.key_for(**ws).as_ref() == Some(key))
                    .filter_map(|ws| self.workspaces.get(ws).map(|w| w.name.clone()))
                    .collect();
                let clone_udid = match (key, &d.target) {
                    (PoolKey::Own(_), Target::IosOwn { udid, .. }) => udid.clone(),
                    _ => None,
                };
                DeviceRow {
                    key: key.clone(),
                    platform: d.platform,
                    name: d.name.clone(),
                    state: match &d.state {
                        State::Open { .. } => "open",
                        State::Opening(_) => "starting",
                        State::ShuttingDown => "shutting down",
                        State::Closed if d.running => "running",
                        State::Closed => "off",
                    },
                    busy: d.busy(),
                    holder: d.leases.holder().map(|h| self.member(*h).name),
                    queue_len: d.leases.waiters().len(),
                    workspaces,
                    clone_udid,
                }
            })
            .collect();
        // Created simulators of workspaces whose device was never opened in this run.
        for (ws, a) in &self.assignments {
            let key = PoolKey::Own(*ws);
            if a.platform == Platform::Ios
                && a.own.is_some()
                && !self.devices.contains_key(&key)
                && let Some(w) = self.workspaces.get(ws)
            {
                rows.push(DeviceRow {
                    key,
                    platform: Platform::Ios,
                    name: clone_name(&w.project, &w.slug),
                    state: "not open",
                    busy: false,
                    holder: None,
                    queue_len: 0,
                    workspaces: vec![w.name.clone()],
                    clone_udid: a.own.clone(),
                });
            }
        }
        rows.sort_by(|a, b| a.name.cmp(&b.name));
        rows
    }

    /// Counts of booted pool devices, for the status bar and Settings.
    pub fn booted_count(&self) -> usize {
        self.devices.values().filter(|d| d.booted()).count()
    }
}
