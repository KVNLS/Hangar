//! Workspace and session lifecycle: create, start, attach, exit, archive, restore, delete.

use super::*;

impl HangarApp {
    /// Creates the worktree and workspace off the main thread, shows it in the focused pane and
    /// starts `agent_id` in it. Resolves once the agent has started (or failed).
    pub(super) fn create_workspace(
        &mut self,
        project_id: ProjectId,
        name: String,
        agent_id: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Task<Option<WorkspaceId>> {
        let prepared = (|| {
            let store = self
                .store
                .clone()
                .ok_or_else(|| anyhow!("the database is not open"))?;
            let project = self
                .projects
                .project(project_id)
                .cloned()
                .ok_or_else(|| anyhow!("unknown project"))?;
            let root = paths::worktrees_root()
                .ok_or_else(|| anyhow!("cannot locate the worktree directory: HOME is not set"))?;
            anyhow::Ok((store, project, root))
        })();
        let (store, project, root) = match prepared {
            Ok(prepared) => prepared,
            Err(err) => {
                self.report(&err, window, cx);
                return Task::ready(None);
            }
        };
        self.creating.insert(project_id);
        cx.notify();

        cx.spawn_in(window, async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn(async move { ops::create_workspace(&store, &project, &name, &root) })
                .await;
            let started = this
                .update_in(cx, |this, window, cx| {
                    this.creating.remove(&project_id);
                    cx.notify();
                    match result {
                        Ok(workspace) => {
                            tracing::info!(
                                name = %workspace.name,
                                branch = %workspace.branch,
                                path = %workspace.worktree_path.display(),
                                "workspace created"
                            );
                            let id = workspace.id;
                            this.projects.upsert_workspace(workspace);
                            this.panes.show(id);
                            this.panes_changed(window, cx);
                            Some((id, this.start_session(id, agent_id, window, cx)))
                        }
                        Err(err) => {
                            this.report(&err, window, cx);
                            None
                        }
                    }
                })
                .ok()
                .flatten();
            let (id, start) = started?;
            start.await;
            Some(id)
        })
    }

    /// Spawns the agent off the main thread and attaches a terminal view to it. Resolves to
    /// whether it started.
    pub(super) fn start_session(
        &mut self,
        workspace_id: WorkspaceId,
        agent_id: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Task<bool> {
        let prepared = (|| {
            let store = self
                .store
                .clone()
                .ok_or_else(|| anyhow!("the database is not open"))?;
            let workspace = self
                .projects
                .workspace(workspace_id)
                .cloned()
                .ok_or_else(|| anyhow!("unknown workspace"))?;
            let adapter =
                agents::preset(&agent_id).ok_or_else(|| anyhow!("unknown agent `{agent_id}`"))?;
            anyhow::Ok((store, workspace, adapter))
        })();
        let (store, workspace, adapter) = match prepared {
            Ok(prepared) => prepared,
            Err(err) => {
                self.report(&err, window, cx);
                return Task::ready(false);
            }
        };
        let path = self.path.as_ref().map(|p| p.value.clone());
        let waker = TerminalWaker::new();
        let hook = waker.hook();
        let agent_name: SharedString = adapter.display_name.clone().into();

        cx.spawn_in(window, async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn(async move {
                    ops::start_session(&store, &adapter, &workspace, path.as_deref(), hook)
                })
                .await;
            this.update_in(cx, |this, window, cx| match result {
                Ok(started) => {
                    this.attach_session(started, agent_name, waker, window, cx);
                    true
                }
                Err(err) => {
                    this.report(&err, window, cx);
                    false
                }
            })
            .unwrap_or(false)
        })
    }

    pub(super) fn attach_session(
        &mut self,
        started: StartedSession,
        agent_name: SharedString,
        waker: TerminalWaker,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let StartedSession {
            session,
            handle,
            record_error,
        } = started;
        let workspace_id = session.workspace_id;
        tracing::info!(
            workspace = %workspace_id,
            session = %session.id,
            program = %handle.program().display(),
            pid = ?handle.session().pid(),
            "session started"
        );
        if let Some(err) = record_error {
            self.report(&err, window, cx);
        }
        // The workspace may have been archived or deleted while the agent was starting.
        if !self.projects.is_active(workspace_id) || self.busy.contains(&workspace_id) {
            if let Err(err) = handle.session().kill() {
                tracing::warn!(%err, "failed to kill agent of a removed workspace");
            }
            let id = session.id;
            self.persist(window, cx, move |store| {
                ops::stop_session(store, id, "workspace closed while starting")
            });
            return;
        }
        let terminal = Arc::clone(handle.session());
        let view =
            cx.new(|cx| TerminalView::attach(terminal, waker, TerminalViewOptions::default(), cx));
        let exit = cx.subscribe_in(
            &view,
            window,
            move |this, _, event: &TerminalEvent, window, cx| match event {
                TerminalEvent::Exited(exit) => this.on_session_exit(workspace_id, exit, window, cx),
            },
        );
        if let Some(previous) = self.sessions.insert(
            workspace_id,
            LiveSession {
                session,
                agent_name,
                handle,
                view,
                activity: Default::default(),
                _exit: exit,
            },
        ) && let Some(id) = previous.stop()
        {
            self.persist(window, cx, move |store| {
                ops::stop_session(store, id, "replaced by a new session")
            });
        }
        if self.panes.slot(self.panes.focused()) == Some(workspace_id) {
            self.focus_terminal(window, cx);
        }
        cx.notify();
    }

    pub(super) fn on_session_exit(
        &mut self,
        workspace_id: WorkspaceId,
        exit: &Result<ExitInfo, String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let status = ops::exit_status(exit);
        let Some(live) = self.sessions.get_mut(&workspace_id) else {
            return;
        };
        tracing::info!(workspace = %workspace_id, session = %live.session.id, ?status, "session ended");
        live.session.status = status.clone();
        let session_id = live.session.id;
        cx.notify();
        self.persist(window, cx, move |store| {
            ops::finish_session(store, session_id, &status).map(drop)
        });
    }

    /// Kills the workspace's agent, if any, and forgets its terminal. Returns the session to
    /// record as stopped and the terminal to wait on.
    pub(super) fn close_session(
        &mut self,
        workspace_id: WorkspaceId,
    ) -> (
        Option<hangar_core::SessionId>,
        Option<Arc<hangar_terminal::TerminalSession>>,
    ) {
        match self.sessions.remove(&workspace_id) {
            Some(live) => {
                let stopped = live.stop();
                (stopped, Some(Arc::clone(live.handle.session())))
            }
            None => (None, None),
        }
    }

    pub(super) fn archive_workspace(
        &mut self,
        workspace_id: WorkspaceId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Task<()> {
        if self.busy.contains(&workspace_id) || !self.projects.is_active(workspace_id) {
            return Task::ready(());
        }
        let Some(store) = self.store.clone() else {
            return Task::ready(());
        };
        let (session, _) = self.close_session(workspace_id);
        self.close_run(workspace_id);
        self.projects
            .set_status(workspace_id, WorkspaceStatus::Archived);
        if self.panes.clear(workspace_id) {
            self.save_layout(window, cx);
        }
        self.busy.insert(workspace_id);
        self.sync_diffs(cx);
        cx.notify();
        cx.spawn_in(window, async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn(async move { ops::archive_workspace(&store, workspace_id, session) })
                .await;
            let _ = this.update_in(cx, |this, window, cx| {
                this.busy.remove(&workspace_id);
                match result {
                    Ok(()) => tracing::info!(workspace = %workspace_id, "workspace archived"),
                    Err(err) => {
                        // The agent is already stopped; show the row where the store still has it.
                        this.projects
                            .set_status(workspace_id, WorkspaceStatus::Active);
                        this.report(&err, window, cx);
                    }
                }
                cx.notify();
            });
        })
    }

    pub(super) fn restore_workspace(
        &mut self,
        workspace_id: WorkspaceId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Task<()> {
        let Some(store) = self.store.clone() else {
            return Task::ready(());
        };
        if !self.busy.insert(workspace_id) {
            return Task::ready(());
        }
        cx.notify();
        cx.spawn_in(window, async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn(async move { ops::restore_workspace(&store, workspace_id) })
                .await;
            let _ = this.update_in(cx, |this, window, cx| {
                this.busy.remove(&workspace_id);
                match result {
                    Ok(()) => {
                        tracing::info!(workspace = %workspace_id, "workspace restored");
                        this.projects
                            .set_status(workspace_id, WorkspaceStatus::Active);
                        this.panes.show(workspace_id);
                        this.panes_changed(window, cx);
                    }
                    Err(err) => this.report(&err, window, cx),
                }
                cx.notify();
            });
        })
    }

    pub(super) fn confirm_delete_workspace(
        &mut self,
        workspace_id: WorkspaceId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.busy.contains(&workspace_id) {
            return;
        }
        let Some(workspace) = self.projects.workspace(workspace_id).cloned() else {
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
                    });
                    true
                })
        });
    }

    pub(super) fn load_delete_summary(
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
    pub(super) fn delete_workspace(
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
        if !self.busy.insert(workspace_id) {
            return Task::ready(false);
        }
        let (session, terminal) = self.close_session(workspace_id);
        self.close_run(workspace_id);
        // Stop watching before the worktree disappears.
        self.sync_diffs(cx);
        cx.notify();
        cx.spawn_in(window, async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn(async move {
                    if let Some(terminal) = terminal
                        && terminal.wait_for_exit(KILL_GRACE).is_none()
                    {
                        tracing::warn!("agent still running after kill; removing worktree anyway");
                    }
                    if let Some(session) = session {
                        ops::stop_session(&store, session, "workspace deleted")?;
                    }
                    ops::delete_workspace(&store, workspace_id, options)
                })
                .await;
            this.update_in(cx, |this, window, cx| {
                this.busy.remove(&workspace_id);
                cx.notify();
                match result {
                    Ok(outcome) => {
                        tracing::info!(
                            workspace = %workspace_id,
                            ?options,
                            branch_error = ?outcome.branch_error,
                            "workspace deleted"
                        );
                        this.projects.remove_workspace(workspace_id);
                        this.forget_run_config(workspace_id);
                        if this.panes.clear(workspace_id) {
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

    pub(super) fn confirm_remove_project(
        &mut self,
        project_id: ProjectId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(entry) = self.projects.entry(project_id) else {
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

    pub(super) fn remove_project(
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
                        let removed = this.projects.remove_project(project_id);
                        let mut cleared = false;
                        for ws in removed.iter().flat_map(|e| &e.workspaces) {
                            this.close_session(ws.id);
                            cleared |= this.panes.clear(ws.id);
                        }
                        if cleared {
                            this.save_layout(window, cx);
                        }
                        this.sync_diffs(cx);
                        if this
                            .new_workspace
                            .as_ref()
                            .is_some_and(|n| n.project_id == project_id)
                        {
                            this.new_workspace = None;
                        }
                    }
                    Err(err) => this.report(&err, window, cx),
                }
                cx.notify();
            });
        })
    }

    pub(super) fn select_workspace(
        &mut self,
        workspace_id: WorkspaceId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.panes.show(workspace_id);
        self.focus_terminal(window, cx);
        self.panes_changed(window, cx);
        cx.notify();
    }

    pub(super) fn focus_pane(&mut self, pane: usize, window: &mut Window, cx: &mut Context<Self>) {
        if self.panes.focused() != pane {
            self.panes.focus(pane);
            self.focus_terminal(window, cx);
            self.panes_changed(window, cx);
            cx.notify();
        }
    }

    pub(super) fn focus_terminal(&self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(id) = self.panes.slot(self.panes.focused()) else {
            return;
        };
        let view = self
            .run_view(id, self.pane_tab(id))
            .or_else(|| self.sessions.get(&id).map(|live| live.view.clone()));
        if let Some(view) = view {
            let handle = view.focus_handle(cx);
            window.focus(&handle, cx);
        }
    }

    /// Switches the workspace's pane between its agent and run terminals.
    pub(super) fn show_pane_tab(
        &mut self,
        id: WorkspaceId,
        tab: run::PaneTab,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.pane_tabs.insert(id, tab);
        if let Some(pane) = (0..PANE_COUNT).find(|p| self.panes.slot(*p) == Some(id)) {
            self.focus_pane(pane, window, cx);
        }
        self.focus_terminal(window, cx);
        cx.notify();
    }

    /// The first active workspace with this slug, for dev hooks.
    pub(super) fn workspace_by_slug(&self, slug: &str) -> Option<WorkspaceId> {
        self.projects
            .entries()
            .iter()
            .flat_map(|e| &e.workspaces)
            .find(|w| w.slug == slug)
            .map(|w| w.id)
    }
}
