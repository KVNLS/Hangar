//! Deleting workspaces and removing projects.

use super::*;

impl HangarApp {
    pub(in crate::shell) fn confirm_delete_workspace(
        &mut self,
        workspace_id: WorkspaceId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.work.busy.contains(&workspace_id) {
            return;
        }
        let Some(workspace) = self.work.projects.workspace(workspace_id).cloned() else {
            return;
        };
        let title: SharedString = format!("Delete workspace “{}”?", workspace.name).into();
        let body = cx.new(|_| DeleteWorkspaceDialog::new(workspace.clone()));
        self.load_delete_summary(workspace, body.clone(), window, cx);

        let app = cx.entity().downgrade();
        window.open_alert_dialog(cx, move |alert, _, _| {
            let body_for_ok = body.clone();
            let app = app.clone();
            alert
                .confirm()
                .title(title.clone())
                .child(body.clone())
                .ok_text("Delete")
                .ok_variant(ButtonVariant::Danger)
                .cancel_text("Cancel")
                .on_ok(move |_, window, cx| {
                    let Some(options) = body_for_ok.read(cx).options() else {
                        // Still reading the status: the user has not seen what would be lost.
                        return false;
                    };
                    let _ = app.update(cx, |this, cx| {
                        this.delete_workspace(workspace_id, options, window, cx)
                            .detach()
                    });
                    true
                })
        });
    }

    pub(in crate::shell) fn load_delete_summary(
        &mut self,
        workspace: Workspace,
        body: Entity<DeleteWorkspaceDialog>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        cx.spawn_in(window, async move |_, cx| {
            let summary = cx
                .background_executor()
                .spawn(async move { ops::delete_summary(&workspace) })
                .await
                .map_err(|err| SharedString::from(format!("{err:#}")));
            body.update(cx, |body, cx| {
                body.set_summary(summary);
                cx.notify();
            });
        })
        .detach();
    }

    /// Stops the agent, then removes the worktree, optional branch and rows in the background.
    pub(in crate::shell) fn delete_workspace(
        &mut self,
        workspace_id: WorkspaceId,
        options: DeleteOptions,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Task<bool> {
        let Some(store) = self.store.clone() else {
            self.report(&anyhow!("the database is not open"), window, cx);
            return Task::ready(false);
        };
        if !self.work.busy.insert(workspace_id) {
            return Task::ready(false);
        }
        let (session, terminal) = self.close_session(workspace_id);
        self.drain_agent_queue(window, cx);
        self.close_run(workspace_id);
        self.close_mcp(workspace_id);
        // Stop watching before the worktree disappears.
        self.sync_diffs(cx);
        cx.notify();
        cx.spawn_in(window, async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn(async move {
                    if let Some(waiter) = terminal
                        && !waiter.wait(KILL_GRACE)
                    {
                        tracing::warn!("agent still running after kill; removing worktree anyway");
                    }
                    if let Some(session) = session {
                        ops::stop_session(&store, session, "workspace deleted", None)?;
                    }
                    let outcome = ops::delete_workspace(&store, workspace_id, options)?;
                    // The saved port goes too, so another workspace can take it.
                    for forgotten in [
                        ops::forget_run_config(&store, workspace_id),
                        ops::forget_chat(&store, workspace_id),
                    ] {
                        if let Err(err) = forgotten {
                            tracing::warn!("{err:#}");
                        }
                    }
                    anyhow::Ok(outcome)
                })
                .await;
            this.update_in(cx, |this, window, cx| {
                this.work.busy.remove(&workspace_id);
                cx.notify();
                match result {
                    Ok(outcome) => {
                        tracing::info!(
                            workspace = %workspace_id,
                            ?options,
                            branch_error = ?outcome.branch_error,
                            "workspace deleted"
                        );
                        this.work.projects.remove_workspace(workspace_id);
                        this.forget_chat(workspace_id);
                        let device = this
                            .devices.pool
                            .update(cx, |pool, cx| pool.delete(workspace_id, cx));
                        cx.spawn(async move |_, _| match device.await {
                            Ok(()) => tracing::info!(workspace = %workspace_id, "workspace device deleted"),
                            Err(err) => tracing::warn!(workspace = %workspace_id, "workspace device not deleted: {err:#}"),
                        })
                        .detach();
                        if this.layout.panes.clear(workspace_id) {
                            this.panes_changed(window, cx);
                        }
                        if let Some(message) = outcome.branch_error {
                            window.push_notification(Notification::warning(message), cx);
                        }
                        true
                    }
                    Err(err) => {
                        this.sync_diffs(cx);
                        this.report(&err, window, cx);
                        false
                    }
                }
            })
            .unwrap_or(false)
        })
    }

    pub(in crate::shell) fn confirm_remove_project(
        &mut self,
        project_id: ProjectId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(entry) = self.work.projects.entry(project_id) else {
            return;
        };
        let name = entry.project.name.clone();
        let active: Vec<String> = entry.active().map(|w| w.name.clone()).collect();
        if !active.is_empty() {
            let message: SharedString = format!(
                "Archive or delete its workspaces first: {}.",
                active.join(", ")
            )
            .into();
            let title: SharedString = format!("“{name}” still has workspaces").into();
            window.open_alert_dialog(cx, move |alert, _, _| {
                alert.title(title.clone()).description(message.clone())
            });
            return;
        }
        let archived = entry.archived().count();
        let mut message = format!(
            "{} stays untouched on disk.",
            entry.project.repo_path.display()
        );
        if archived > 0 {
            message.push_str(&format!(
                " {archived} archived workspace(s) will be forgotten; their worktrees and \
                 branches are left on disk."
            ));
        }
        let message: SharedString = message.into();
        let title: SharedString = format!("Remove “{name}” from Hangar?").into();
        let app = cx.entity().downgrade();
        window.open_alert_dialog(cx, move |alert, _, _| {
            let app = app.clone();
            alert
                .confirm()
                .title(title.clone())
                .description(message.clone())
                .ok_text("Remove")
                .ok_variant(ButtonVariant::Danger)
                .cancel_text("Cancel")
                .on_ok(move |_, window, cx| {
                    let _ = app.update(cx, |this, cx| {
                        this.remove_project(project_id, window, cx).detach();
                    });
                    true
                })
        });
    }

    pub(in crate::shell) fn remove_project(
        &mut self,
        project_id: ProjectId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Task<()> {
        let Some(store) = self.store.clone() else {
            return Task::ready(());
        };
        cx.spawn_in(window, async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn(async move { ops::remove_project(&store, project_id) })
                .await;
            let _ = this.update_in(cx, |this, window, cx| {
                match result {
                    Ok(()) => {
                        tracing::info!(project = %project_id, "project removed");
                        let removed = this.work.projects.remove_project(project_id);
                        let mut cleared = false;
                        for ws in removed.iter().flat_map(|e| &e.workspaces) {
                            this.close_session(ws.id);
                            cleared |= this.layout.panes.clear(ws.id);
                            // Its simulator clone and assignment go with it, as on delete.
                            let id = ws.id;
                            let deleted =
                                this.devices.pool.update(cx, |pool, cx| pool.delete(id, cx));
                            cx.spawn(async move |_, _| {
                                if let Err(err) = deleted.await {
                                    tracing::warn!(workspace = %id, "device not deleted: {err:#}");
                                }
                            })
                            .detach();
                        }
                        if cleared {
                            this.save_layout(window, cx);
                        }
                        this.drain_agent_queue(window, cx);
                        this.sync_diffs(cx);
                        if this
                            .work
                            .new_workspace
                            .as_ref()
                            .is_some_and(|n| n.project_id == project_id)
                        {
                            this.work.new_workspace = None;
                        }
                    }
                    Err(err) => this.report(&err, window, cx),
                }
                cx.notify();
            });
        })
    }
}
