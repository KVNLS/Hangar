//! Archiving and restoring workspaces.

use super::*;

impl HangarApp {
    pub(in crate::shell) fn archive_workspace(
        &mut self,
        workspace_id: WorkspaceId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Task<()> {
        if self.work.busy.contains(&workspace_id) || !self.work.projects.is_active(workspace_id) {
            return Task::ready(());
        }
        let Some(store) = self.store.clone() else {
            return Task::ready(());
        };
        let changes = self.diff_stat(workspace_id, cx);
        let (session, _) = self.close_session(workspace_id);
        self.close_run(workspace_id);
        self.close_mcp(workspace_id);
        self.work
            .projects
            .set_status(workspace_id, WorkspaceStatus::Archived);
        if self.layout.panes.clear(workspace_id) {
            self.save_layout(window, cx);
        }
        self.work.busy.insert(workspace_id);
        self.drain_agent_queue(window, cx);
        self.sync_diffs(cx);
        self.sync_device_focus(window, cx);
        let device = self
            .device_pool
            .update(cx, |pool, cx| pool.archive(workspace_id, cx));
        cx.notify();
        cx.spawn_in(window, async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn(async move {
                    ops::archive_workspace(&store, workspace_id, session, changes)
                })
                .await;
            match device.await {
                Ok(()) => tracing::info!(workspace = %workspace_id, "workspace device shut down"),
                Err(err) => {
                    tracing::warn!(workspace = %workspace_id, "workspace device not shut down: {err:#}")
                }
            }
            let _ = this.update_in(cx, |this, window, cx| {
                this.work.busy.remove(&workspace_id);
                match result {
                    Ok(()) => {
                        tracing::info!(workspace = %workspace_id, "workspace archived");
                        this.refresh_done_today(cx);
                    }
                    Err(err) => {
                        // The agent is already stopped; show the row where the store still has it.
                        this.work.projects
                            .set_status(workspace_id, WorkspaceStatus::Active);
                        this.report(&err, window, cx);
                    }
                }
                cx.notify();
            });
        })
    }

    pub(in crate::shell) fn restore_workspace(
        &mut self,
        workspace_id: WorkspaceId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Task<()> {
        let Some(store) = self.store.clone() else {
            return Task::ready(());
        };
        if !self.work.busy.insert(workspace_id) {
            return Task::ready(());
        }
        cx.notify();
        cx.spawn_in(window, async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn(async move { ops::restore_workspace(&store, workspace_id) })
                .await;
            let _ = this.update_in(cx, |this, window, cx| {
                this.work.busy.remove(&workspace_id);
                match result {
                    Ok(()) => {
                        tracing::info!(workspace = %workspace_id, "workspace restored");
                        this.work
                            .projects
                            .set_status(workspace_id, WorkspaceStatus::Active);
                        this.layout.panes.show(workspace_id);
                        this.panes_changed(window, cx);
                    }
                    Err(err) => this.report(&err, window, cx),
                }
                cx.notify();
            });
        })
    }
}
