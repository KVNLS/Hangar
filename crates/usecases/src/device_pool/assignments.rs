//! Which device each workspace uses: its platform and base, and the workspace on screen.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::time::Instant;

use hangar_core::WorkspaceId;
use hangar_core::device::{DeviceKey, Platform, Target, clone_name};

use super::policy::{Assignment, LeaseQueue};
use super::shutdown::Replaced;
use super::{PoolDevice, PoolKey, PoolMachine, Save, Session, State, WorkspaceRef};

/// What a waiting call gets when its workspace is given another device.
const REASSIGNED: &str =
    "this workspace was given another device while this call waited; call again";

impl<S: Session> PoolMachine<S> {
    /// Records the workspace (name, worktree) for later calls that only have its id.
    pub fn track(&mut self, ws: WorkspaceRef) {
        self.workspaces.insert(ws.id, ws);
    }

    pub fn assignment(&self, ws: WorkspaceId) -> Option<&Assignment> {
        self.assignments.get(&ws)
    }

    /// The last platform and base chosen in the New workspace sheet.
    pub fn last_target(&self) -> Option<&DeviceKey> {
        self.last_target.as_ref()
    }

    /// The name of a tracked workspace.
    pub fn workspace_name(&self, ws: WorkspaceId) -> Option<&str> {
        self.workspaces.get(&ws).map(|w| w.name.as_str())
    }

    /// The worktree of a tracked workspace.
    pub fn workspace_root(&self, ws: WorkspaceId) -> Option<PathBuf> {
        self.workspaces.get(&ws).map(|w| w.root.clone())
    }

    /// The base device of each platform chosen in Settings.
    pub fn bases(&self) -> &BTreeMap<Platform, String> {
        &self.bases
    }

    /// Assigns a platform and base device to a workspace (New workspace sheet, device panel).
    /// A different base replaces the workspace's own device: it is shut down and, when it is a
    /// simulator the pool created, deleted (returned for the caller to carry out). The
    /// workspace leaves the device it used ([`Self::unbind`]).
    pub fn assign(
        &mut self,
        ws: WorkspaceId,
        platform: Platform,
        base: String,
        now: Instant,
    ) -> Option<Replaced<S>> {
        let previous = self.assignments.get(&ws).cloned();
        if previous
            .as_ref()
            .is_some_and(|a| a.platform == platform && a.base == base)
        {
            return None;
        }
        tracing::info!(target: "device_pool", workspace = %ws, platform = platform.label(), %base, "device assigned");
        self.unbind(ws, now, REASSIGNED);
        let assignment = Assignment::new(platform, base.clone());
        self.assignments.insert(ws, assignment.clone());
        self.save(Save::Assignment(ws, assignment));
        let last = DeviceKey { platform, id: base };
        self.last_target = Some(last.clone());
        self.save(Save::LastTarget(last));
        let replaced = previous.map(|previous| {
            let key = PoolKey::Own(ws, previous.platform);
            let shutdown = self.shutdown(&key);
            self.remove_device(&key);
            Replaced {
                shutdown,
                clone: (previous.platform == Platform::Ios)
                    .then_some(previous.own)
                    .flatten(),
            }
        });
        self.refocus();
        replaced
    }

    /// The assignment resolved off the UI thread for `ws`; another call may have resolved it
    /// meanwhile, and the first one is kept.
    pub fn resolved(&mut self, ws: WorkspaceId, assignment: Assignment) -> Assignment {
        let a = self
            .assignments
            .entry(ws)
            .or_insert_with(|| assignment)
            .clone();
        self.refocus();
        a
    }

    /// Name of the device the workspace uses, once the pool knows it.
    pub fn device_name(&self, ws: WorkspaceId) -> Option<&str> {
        self.name_of(&self.key_for(ws)?)
    }

    /// Name of the pool device `key`, once the pool knows it.
    pub fn name_of(&self, key: &PoolKey) -> Option<&str> {
        self.devices.get(key).map(|d| d.name.as_str())
    }

    /// Which device the workspace uses now: the one it is bound to (possibly another
    /// workspace's), else its own.
    pub fn key_for(&self, ws: WorkspaceId) -> Option<PoolKey> {
        if let Some(key) = self.bindings.get(&ws) {
            return Some(key.clone());
        }
        let a = self.assignments.get(&ws)?;
        Some(self.key_of(ws, a))
    }

    /// The workspace's own device given its assignment (the base device in single-device mode).
    pub fn key_of(&self, ws: WorkspaceId, a: &Assignment) -> PoolKey {
        if self.single_device(a.platform) {
            PoolKey::Base(DeviceKey {
                platform: a.platform,
                id: a.base.clone(),
            })
        } else {
            PoolKey::Own(ws, a.platform)
        }
    }

    pub(super) fn entry(&mut self, key: &PoolKey) -> Option<&mut PoolDevice<S>> {
        if !self.devices.contains_key(key) {
            let device = self.new_device(key)?;
            self.devices.insert(key.clone(), device);
        }
        self.devices.get_mut(key)
    }

    fn new_device(&self, key: &PoolKey) -> Option<PoolDevice<S>> {
        let (platform, name, target) = match key {
            PoolKey::Base(k) => (
                k.platform,
                k.id.clone(),
                match k.platform {
                    Platform::Ios => Target::Ios { udid: k.id.clone() },
                    Platform::Android => Target::Android {
                        avd: k.id.clone(),
                        serial: None,
                        shared: true,
                    },
                },
            ),
            PoolKey::Own(ws, platform) => {
                let a = self
                    .assignments
                    .get(ws)
                    .filter(|a| a.platform == *platform)?;
                let w = self.workspaces.get(ws)?;
                let name = clone_name(&w.project, &w.slug);
                let target = match a.platform {
                    Platform::Ios => Target::IosOwn {
                        udid: a.own.clone(),
                        base: a.base.clone(),
                        name: name.clone(),
                    },
                    Platform::Android => Target::Android {
                        avd: a.base.clone(),
                        serial: a.serial.clone(),
                        shared: false,
                    },
                };
                (a.platform, name, target)
            }
        };
        Some(PoolDevice {
            platform,
            name,
            target,
            state: State::Closed,
            running: false,
            last_used: 0,
            leases: LeaseQueue::default(),
            error: None,
            conflict: None,
            installed_by: None,
        })
    }

    pub(super) fn mark_used(&mut self, key: &PoolKey) {
        self.tick += 1;
        let tick = self.tick;
        if let Some(d) = self.devices.get_mut(key) {
            d.last_used = tick;
        }
    }

    /// The workspace on screen: its device is never shut down to make room. Whether its
    /// assignment should now be resolved (tracked but not assigned yet).
    pub fn set_focused(&mut self, ws: Option<WorkspaceId>) -> bool {
        if self.focused == ws {
            return false;
        }
        self.focused = ws;
        let resolve = ws.is_some_and(|ws| {
            !self.assignments.contains_key(&ws) && self.workspaces.contains_key(&ws)
        });
        self.refocus();
        resolve
    }

    fn refocus(&mut self) {
        if let Some(key) = self.focused.and_then(|ws| self.key_for(ws)) {
            self.mark_used(&key);
        }
    }

    pub(super) fn focused_key(&self) -> Option<PoolKey> {
        self.focused.and_then(|ws| self.key_for(ws))
    }
}
