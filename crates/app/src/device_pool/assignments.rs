//! Which device each workspace uses: its platform and base, and the workspace on screen.

use anyhow::anyhow;
use gpui_kit::*;
use hangar_core::WorkspaceId;

use crate::device_panel::{DeviceKey, Platform};
use crate::setting_keys;

use super::backend::{self, Target};
use super::policy::{Assignment, LeaseQueue};
use super::{DevicePool, PoolDevice, PoolKey, State, WorkspaceRef, default_assignment};

impl DevicePool {
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

    /// Assigns a platform and base device to a workspace (New workspace sheet, device panel).
    /// A different base replaces the workspace's own device: it is shut down and, when it is
    /// a simulator the pool created, deleted.
    pub fn assign(
        &mut self,
        ws: WorkspaceId,
        platform: Platform,
        base: String,
        cx: &mut Context<Self>,
    ) {
        let previous = self.assignments.get(&ws).cloned();
        if previous
            .as_ref()
            .is_some_and(|a| a.platform == platform && a.base == base)
        {
            return;
        }
        tracing::info!(target: "device_pool", workspace = %ws, platform = platform.label(), %base, "device assigned");
        let assignment = Assignment::new(platform, base.clone());
        self.assignments.insert(ws, assignment.clone());
        self.save(setting_keys::device_assignment(ws), assignment, cx);
        let last = DeviceKey { platform, id: base };
        self.last_target = Some(last.clone());
        self.save(setting_keys::LAST_DEVICE_TARGET, last, cx);
        if let Some(previous) = previous {
            self.drop_own_device(ws, previous, cx);
        }
        self.refocus(cx);
        cx.notify();
    }

    fn drop_own_device(&mut self, ws: WorkspaceId, previous: Assignment, cx: &mut Context<Self>) {
        let key = PoolKey::Own(ws);
        let shutdown = self.shutdown(&key, cx);
        self.devices.remove(&key);
        let clone = (previous.platform == Platform::Ios)
            .then_some(previous.own)
            .flatten();
        cx.spawn(async move |_, cx| {
            if let Err(err) = shutdown.await {
                tracing::warn!(target: "device_pool", "shutdown of the replaced device failed: {err:#}");
            }
            if let Some(udid) = clone {
                let deleted = cx
                    .background_executor()
                    .spawn(async move { backend::delete_clone(&udid) })
                    .await;
                if let Err(err) = deleted {
                    tracing::warn!(target: "device_pool", "could not delete the replaced simulator: {err:#}");
                }
            }
        })
        .detach();
    }

    /// The workspace's assignment, loading or creating (and saving) it on first use: the
    /// platform of the last choice when the repository builds for it, else the first platform it
    /// builds for; the platform's base device from Settings, else a running one, else the newest.
    pub(super) fn resolve_assignment(
        &mut self,
        ws: WorkspaceId,
        cx: &mut Context<Self>,
    ) -> Task<anyhow::Result<Assignment>> {
        if let Some(a) = self.assignments.get(&ws) {
            return Task::ready(Ok(a.clone()));
        }
        let Some(store) = self.store.clone() else {
            return Task::ready(Err(anyhow!("the database is not open")));
        };
        let Some(root) = self.workspaces.get(&ws).map(|w| w.root.clone()) else {
            return Task::ready(Err(anyhow!("unknown workspace")));
        };
        let bases = self.bases.clone();
        cx.spawn(async move |this, cx| {
            let resolved = cx
                .background_executor()
                .spawn(async move { default_assignment(&store, ws, &root, &bases) })
                .await?;
            this.update(cx, |pool, cx| {
                // Another call may have resolved it meanwhile; keep the first.
                let a = pool
                    .assignments
                    .entry(ws)
                    .or_insert_with(|| resolved.clone())
                    .clone();
                pool.refocus(cx);
                cx.notify();
                a
            })
        })
    }

    /// The workspace's assignment, resolved (and saved) on first use.
    pub fn assignment_for(
        &mut self,
        ws: WorkspaceId,
        cx: &mut Context<Self>,
    ) -> Task<anyhow::Result<Assignment>> {
        self.resolve_assignment(ws, cx)
    }

    /// Name of the device the workspace uses, once the pool knows it.
    pub fn device_name(&self, ws: WorkspaceId) -> Option<&str> {
        self.devices
            .get(&self.key_for(ws)?)
            .map(|d| d.name.as_str())
    }

    /// Which device the workspace uses now.
    pub fn key_for(&self, ws: WorkspaceId) -> Option<PoolKey> {
        let a = self.assignments.get(&ws)?;
        Some(self.key_of(ws, a))
    }

    pub(super) fn key_of(&self, ws: WorkspaceId, a: &Assignment) -> PoolKey {
        if self.single_device(a.platform) {
            PoolKey::Base(DeviceKey {
                platform: a.platform,
                id: a.base.clone(),
            })
        } else {
            PoolKey::Own(ws)
        }
    }

    pub(super) fn entry(&mut self, key: &PoolKey) -> Option<&mut PoolDevice> {
        if !self.devices.contains_key(key) {
            let device = self.new_device(key)?;
            self.devices.insert(key.clone(), device);
        }
        self.devices.get_mut(key)
    }

    fn new_device(&self, key: &PoolKey) -> Option<PoolDevice> {
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
            PoolKey::Own(ws) => {
                let a = self.assignments.get(ws)?;
                let w = self.workspaces.get(ws)?;
                let name = backend::clone_name(&w.project, &w.slug);
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
        })
    }

    pub(super) fn touch(&mut self, key: &PoolKey) {
        self.tick += 1;
        let tick = self.tick;
        if let Some(d) = self.devices.get_mut(key) {
            d.last_used = tick;
        }
    }

    /// The workspace on screen: its device is never shut down to make room.
    pub fn set_focused(&mut self, ws: Option<WorkspaceId>, cx: &mut Context<Self>) {
        if self.focused == ws {
            return;
        }
        self.focused = ws;
        if let Some(ws) = ws
            && !self.assignments.contains_key(&ws)
            && self.workspaces.contains_key(&ws)
        {
            self.resolve_assignment(ws, cx).detach();
        }
        self.refocus(cx);
        cx.notify();
    }

    fn refocus(&mut self, cx: &mut Context<Self>) {
        let _ = cx;
        if let Some(key) = self.focused.and_then(|ws| self.key_for(ws)) {
            self.touch(&key);
        }
    }

    pub(super) fn focused_key(&self) -> Option<PoolKey> {
        self.focused.and_then(|ws| self.key_for(ws))
    }
}
