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

    /// The workspace's active assignment: the platform attached now.
    pub fn assignment(&self, ws: WorkspaceId) -> Option<&Assignment> {
        self.assignments.get(&ws)
    }

    /// The workspace's assignment of its other platform, kept for switching back.
    pub fn parked(&self, ws: WorkspaceId) -> Option<&Assignment> {
        let active = self.assignments.get(&ws).map(|a| a.platform);
        self.parked.get(&ws).filter(|p| Some(p.platform) != active)
    }

    /// The workspace's assignment of `platform`: the active one, else the parked one.
    pub fn assignment_of(&self, ws: WorkspaceId, platform: Platform) -> Option<&Assignment> {
        self.assignments
            .get(&ws)
            .filter(|a| a.platform == platform)
            .or_else(|| self.parked(ws).filter(|a| a.platform == platform))
    }

    /// Every assignment with an own device: active ones, then parked ones.
    pub(super) fn own_assignments(&self) -> impl Iterator<Item = (WorkspaceId, &Assignment)> {
        let parked = self
            .parked
            .keys()
            .filter_map(|ws| self.parked(*ws).map(|a| (*ws, a)));
        self.assignments
            .iter()
            .map(|(ws, a)| (*ws, a))
            .chain(parked)
    }

    /// Changes the assignment of `ws` for `platform` (active or parked) and saves it if it
    /// changed.
    pub(super) fn edit_assignment(
        &mut self,
        ws: WorkspaceId,
        platform: Platform,
        edit: impl FnOnce(&mut Assignment),
    ) {
        let parked = self.assignment(ws).is_none_or(|a| a.platform != platform);
        let map = if parked {
            &mut self.parked
        } else {
            &mut self.assignments
        };
        let Some(a) = map.get_mut(&ws).filter(|a| a.platform == platform) else {
            return;
        };
        let before = a.clone();
        edit(a);
        if *a == before {
            return;
        }
        let a = a.clone();
        self.save(if parked {
            Save::Parked(ws, a)
        } else {
            Save::Assignment(ws, a)
        });
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
    /// simulator the pool created, deleted (returned for the caller to carry out). A parked
    /// assignment of the new platform is taken back ([`Self::switch_platform`] keeps the old
    /// one instead). The workspace leaves the device it used ([`Self::unbind`]). Refused while a
    /// call of another workspace uses an own device of `ws` it replaces; other workspaces that
    /// only reserve or wait for it lose it.
    pub fn assign(
        &mut self,
        ws: WorkspaceId,
        platform: Platform,
        base: String,
        now: Instant,
    ) -> Result<Option<Replaced<S>>, String> {
        let previous = self.assignments.get(&ws).cloned();
        if previous
            .as_ref()
            .is_some_and(|a| a.platform == platform && a.base == base)
        {
            return Ok(None);
        }
        let mut assignment = Assignment::new(platform, base.clone());
        self.check_replaceable(ws, previous.as_ref().map(|a| PoolKey::Own(ws, a.platform)))?;
        self.check_replaceable(ws, self.replaced_parked(ws, &assignment))?;
        tracing::info!(target: "device_pool", workspace = %ws, platform = platform.label(), %base, "device assigned");
        self.unbind(ws, now, REASSIGNED);
        let unparking = self.parked(ws).is_some_and(|p| p.platform == platform);
        let unparked = self.unpark(ws, &mut assignment, now);
        if unparking {
            self.save(Save::Unparked(ws));
        }
        self.assignments.insert(ws, assignment.clone());
        self.save(Save::Assignment(ws, assignment));
        let last = DeviceKey { platform, id: base };
        self.last_target = Some(last.clone());
        self.save(Save::LastTarget(last));
        let replaced = previous.map(|previous| self.drop_own(ws, &previous, now));
        self.enforce_limit();
        self.refocus();
        Ok(match (replaced, unparked) {
            (Some(a), Some(b)) => Some(self.merge_replaced(a, b)),
            (a, b) => a.or(b),
        })
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
                let a = self.assignment_of(*ws, *platform)?;
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

    pub(super) fn refocus(&mut self) {
        if let Some(key) = self.focused.and_then(|ws| self.key_for(ws)) {
            self.mark_used(&key);
        }
    }

    pub(super) fn focused_key(&self) -> Option<PoolKey> {
        self.focused.and_then(|ws| self.key_for(ws))
    }
}

#[cfg(test)]
mod tests {
    use crate::device_pool::fixtures::*;
    use crate::device_pool::*;

    #[test]
    fn a_new_base_replaces_the_own_device() {
        let w = workspace("a");
        let mut a = ios("OLD");
        a.own = Some("CLONE".into());
        let mut m = pool(Limit::Auto, &[(&w, a)]);
        let now = Instant::now();
        let key = PoolKey::Own(w.id, Platform::Ios);
        let token = wait(m.open(&key, now));
        let probe = step(&m.take_commands());
        let replaced = m
            .assign(w.id, Platform::Ios, "NEW".into(), Instant::now())
            .unwrap()
            .unwrap();
        assert!(replaced.shutdown.is_none(), "not running yet");
        assert_eq!(replaced.clone.as_deref(), Some("CLONE"));
        let commands = m.take_commands();
        assert_eq!(
            replies(&commands),
            [(token, Err(OpenError::Failed(STOPPED.into())))]
        );
        assert!(commands.iter().any(|c| matches!(
            c,
            Command::Save(Save::LastTarget(k)) if k.id == "NEW"
        )));
        assert_eq!(m.assignment(w.id).map(|a| a.base.as_str()), Some("NEW"));
        // The old bring-up's results land nowhere.
        m.step_done(&key, probe.attempt, StepDone::Probed(Ok(None)), now);
        assert!(m.take_commands().is_empty());
        assert!(
            m.assign(w.id, Platform::Ios, "NEW".into(), Instant::now())
                .unwrap()
                .is_none()
        );
    }

    fn android_base(id: &str) -> PoolKey {
        PoolKey::Base(DeviceKey {
            platform: Platform::Android,
            id: id.into(),
        })
    }

    #[test]
    fn a_new_assignment_leaves_the_device_it_shared() {
        let (mut m, [a, b, c], x, t) = shared_base();
        assert_eq!(m.request_device(a.id, t).unwrap().key, x);
        let for_b = queued(m.request_device(b.id, t).map(|s| s.lease));
        let for_c = queued(m.request_device(c.id, t).map(|s| s.lease));
        m.take_commands();
        m.assign(b.id, Platform::Android, "P".into(), t).unwrap();
        let commands = m.take_commands();
        assert_eq!(failed(&commands), [for_b]);
        assert!(grants(&commands).is_empty(), "a keeps the device");
        let p = android_base("P");
        assert_eq!(m.key_for(b.id), Some(p.clone()));
        let start = m.request_device(b.id, t).unwrap();
        assert_eq!((&start.key, start.lease), (&p, LeaseStart::Granted));
        assert_bound_to_held(&m);
        // The holder moves during a call: the call finishes on X, then X passes on.
        m.assign(a.id, Platform::Android, "Q".into(), t).unwrap();
        assert!(grants(&m.take_commands()).is_empty(), "a call is in flight");
        let q = android_base("Q");
        assert_eq!(m.key_for(a.id), Some(q.clone()));
        assert_eq!(m.request_device(a.id, t).unwrap().key, q);
        assert_bound_to_held(&m);
        m.release_lease(&x, a.id, t + secs(1));
        assert_eq!(grants(&m.take_commands()), [(for_c, c.id, true)]);
        // A stale release of the old call changes nothing on either device.
        m.release_lease(&x, a.id, t + secs(2));
        assert!(m.take_commands().is_empty());
        assert_eq!(m.held_by_other(a.id), None);
        assert_eq!(
            m.sharing(c.id, t).and_then(|s| s.holder).map(|h| h.id),
            Some(c.id)
        );
        assert_bound_to_held(&m);
    }

    #[test]
    fn assigning_the_parked_platform_takes_the_parked_assignment_back() {
        let a = workspace("a");
        let mut m = pool(Limit::Auto, &[(&a, ios_own("B", "CLONE"))]);
        let t = Instant::now();
        let android_key = own(&a, Platform::Android);
        done(m.switch_platform(a.id, android("P"), SwitchBy::Agent, t));
        open_running(&mut m, &android_key, "emulator-5554", t);
        // Another base: the parked simulator is deleted, the Android device shut down.
        m.take_commands();
        let replaced = m
            .assign(a.id, Platform::Ios, "C".into(), t)
            .unwrap()
            .unwrap();
        assert_eq!(replaced.clone.as_deref(), Some("CLONE"));
        assert!(replaced.shutdown.is_none(), "the simulator never ran");
        let commands = m.take_commands();
        assert!(commands.iter().any(|c| matches!(
            c,
            Command::Shutdown(job) if job.key == android_key
        )));
        assert!(
            saves(&commands).contains(&&Save::Unparked(a.id)),
            "{commands:?}"
        );
        assert_eq!(m.assignment(a.id), Some(&ios("C")));
        assert_eq!(m.parked(a.id), None);
        // The same base keeps the parked simulator.
        let mut m = pool(Limit::Auto, &[(&a, ios_own("B", "CLONE"))]);
        done(m.switch_platform(a.id, android("P"), SwitchBy::Agent, t));
        let replaced = m
            .assign(a.id, Platform::Ios, "B".into(), t)
            .unwrap()
            .unwrap();
        assert_eq!(replaced.clone, None);
        assert_eq!(m.assignment(a.id), Some(&ios_own("B", "CLONE")));
        assert_eq!(m.parked(a.id), None);
    }
}
