//! Which device each workspace uses: its platform and base, and the workspace on screen.

use anyhow::anyhow;
use gpui_kit::*;
use hangar_core::WorkspaceId;
use hangar_usecases::device_pool::Replaced;

use crate::adapters;
use crate::device_panel::{DeviceKey, Platform};

use super::policy::Assignment;
use super::{DevicePool, PoolKey, WorkspaceRef, default_assignment};

impl DevicePool {
    /// Records the workspace (name, worktree) for later calls that only have its id.
    pub fn track(&mut self, ws: WorkspaceRef) {
        self.machine.track(ws);
    }

    pub fn assignment(&self, ws: WorkspaceId) -> Option<&Assignment> {
        self.machine.assignment(ws)
    }

    /// The last platform and base chosen in the New workspace sheet.
    pub fn last_target(&self) -> Option<&DeviceKey> {
        self.machine.last_target()
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
        if let Some(Replaced { shutdown, clone }) = self.machine.assign(ws, platform, base) {
            let shutdown = match shutdown {
                Some(job) => self.run_shutdown(job, cx),
                None => Task::ready(Ok(())),
            };
            cx.spawn(async move |_, cx| {
                if let Err(err) = shutdown.await {
                    tracing::warn!(target: "device_pool", "shutdown of the replaced device failed: {err:#}");
                }
                if let Some(udid) = clone {
                    let deleted = cx
                        .background_executor()
                        .spawn(async move {
                            adapters::device_catalog(Platform::Ios).delete_clone(&udid)
                        })
                        .await;
                    if let Err(err) = deleted {
                        tracing::warn!(target: "device_pool", "could not delete the replaced simulator: {:#}", anyhow::Error::from(err));
                    }
                }
            })
            .detach();
        }
        self.run(cx);
        cx.notify();
    }

    /// The workspace's assignment, loading or creating (and saving) it on first use: the
    /// platform of the last choice when the repository builds for it, else the first platform it
    /// builds for; the platform's base device from Settings, else a running one, else the newest.
    pub(super) fn resolve_assignment(
        &mut self,
        ws: WorkspaceId,
        cx: &mut Context<Self>,
    ) -> Task<anyhow::Result<Assignment>> {
        if let Some(a) = self.machine.assignment(ws) {
            return Task::ready(Ok(a.clone()));
        }
        let Some(store) = self.store.clone() else {
            return Task::ready(Err(anyhow!("the database is not open")));
        };
        let Some(root) = self.machine.workspace_root(ws) else {
            return Task::ready(Err(anyhow!("unknown workspace")));
        };
        let bases = self.machine.bases().clone();
        cx.spawn(async move |this, cx| {
            let resolved = cx
                .background_executor()
                .spawn(async move { default_assignment(&store, ws, &root, &bases) })
                .await?;
            this.update(cx, |pool, cx| {
                let a = pool.machine.resolved(ws, resolved);
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
        self.machine.device_name(ws)
    }

    /// Which device the workspace uses now.
    pub fn key_for(&self, ws: WorkspaceId) -> Option<PoolKey> {
        self.machine.key_for(ws)
    }

    /// The workspace on screen: its device is never shut down to make room.
    pub fn set_focused(&mut self, ws: Option<WorkspaceId>, cx: &mut Context<Self>) {
        if self.machine.set_focused(ws)
            && let Some(ws) = ws
        {
            self.resolve_assignment(ws, cx).detach();
        }
        cx.notify();
    }
}
