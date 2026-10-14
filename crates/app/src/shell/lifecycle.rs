//! Workspace and session lifecycle: create, start, attach, exit, archive, restore, delete.

use super::*;

impl HangarApp {
    /// Creates the worktree and workspace off the main thread, assigns it `target` as its base
    /// device (else the default on first use), shows it in the focused pane and starts
    /// `agent_id` in it. Resolves once the agent has started (or failed).
    /// `branches`: the base to start from and the new branch's name, each `None` for the
    /// default (see [`ops::create_workspace_from`]).
    #[allow(clippy::too_many_arguments)]
    pub(super) fn create_workspace(
        &mut self,
        project_id: ProjectId,
        name: String,
        agent_id: String,
        target: Option<crate::device_panel::DeviceKey>,
        branches: (Option<String>, Option<String>),
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Task<Option<WorkspaceId>> {
        let prepared = (|| {
            let store = self
                .store
                .clone()
                .ok_or_else(|| anyhow!("the database is not open"))?;
            let project = self
                .work
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
        self.work.creating.insert(project_id);
        cx.notify();

        cx.spawn_in(window, async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn(async move {
                    let (base, branch) = branches;
                    ops::create_workspace_from(
                        &store,
                        &project,
                        &name,
                        &root,
                        base.as_deref(),
                        branch.as_deref(),
                    )
                })
                .await;
            let started = this
                .update_in(cx, |this, window, cx| {
                    this.work.creating.remove(&project_id);
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
                            this.work.projects.upsert_workspace(workspace);
                            this.track_workspace(id, cx);
                            if let Some(target) = target {
                                this.device_pool.update(cx, |pool, cx| {
                                    pool.assign(id, target.platform, target.id, cx)
                                });
                            }
                            this.layout.panes.show(id);
                            this.panes_changed(window, cx);
                            Some((id, this.start_session(id, agent_id, false, window, cx)))
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

    /// Spawns the agent off the main thread and attaches a terminal (or chat) view to it. With
    /// `resume`, a structured agent continues the workspace's saved conversation when it ran
    /// with the same preset. Resolves to whether it started.
    pub(super) fn start_session(
        &mut self,
        workspace_id: WorkspaceId,
        agent_id: String,
        resume: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Task<bool> {
        let prepared = (|| {
            let store = self
                .store
                .clone()
                .ok_or_else(|| anyhow!("the database is not open"))?;
            let workspace = self
                .work
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
        let path = self.agent_setup.path.as_ref().map(|p| p.value.clone());
        if let Some(message) = self.ollama_unavailable(&agent_id) {
            window.push_notification(Notification::warning(message), cx);
        }
        // Without the device tools the agent still runs: say why, don't block it.
        let extras = self
            .agent_extras(adapter.id(), &workspace, cx)
            .unwrap_or_else(|err| {
                tracing::warn!("agent starts without device tools: {err:#}");
                Default::default()
            });
        let waker = TerminalWaker::new();
        let hook = waker.hook();
        let agent_name: SharedString = adapter.display_name().to_owned().into();
        let resume_id = resume
            .then(|| self.resumable_chat(workspace_id, &agent_id))
            .flatten();

        cx.spawn_in(window, async move |this, cx| {
            let started = cx
                .background_executor()
                .spawn(async move {
                    match &adapter {
                        AgentPreset::Pty(adapter) => ops::start_session_with(
                            &store,
                            adapter,
                            &workspace,
                            path.as_deref(),
                            hook,
                            &extras,
                        )
                        .map(Started::Terminal),
                        AgentPreset::Acp(adapter) => ops::start_acp_session(
                            &store,
                            adapter,
                            &workspace,
                            path.as_deref(),
                            hook,
                            &extras,
                            resume_id.as_deref(),
                        )
                        .map(Started::Chat),
                    }
                })
                .await;
            this.update_in(cx, |this, window, cx| match started {
                Ok(Started::Terminal(started)) => {
                    this.attach_session(started, agent_name, waker, window, cx);
                    true
                }
                Ok(Started::Chat(started)) => {
                    this.attach_chat_session(started, agent_name, waker, window, cx);
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

    /// Whether a freshly started agent may stay: the workspace may have been archived or
    /// deleted while it was starting. Records the session as stopped when it may not.
    fn keep_started(
        &mut self,
        workspace_id: WorkspaceId,
        session: &Session,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        if self.work.projects.is_active(workspace_id) && !self.work.busy.contains(&workspace_id) {
            return true;
        }
        let id = session.id;
        self.persist(window, cx, move |store| {
            ops::stop_session(store, id, "workspace closed while starting")
        });
        false
    }

    fn insert_live_session(
        &mut self,
        workspace_id: WorkspaceId,
        live: LiveSession,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(previous) = self.live.sessions.insert(workspace_id, live)
            && let Some(id) = previous.stop()
        {
            self.persist(window, cx, move |store| {
                ops::stop_session(store, id, "replaced by a new session")
            });
        }
        if self.layout.panes.slot(self.layout.panes.focused()) == Some(workspace_id) {
            self.focus_terminal(window, cx);
        }
        cx.notify();
    }

    pub(super) fn attach_chat_session(
        &mut self,
        started: ops::StartedAcpSession,
        agent_name: SharedString,
        waker: TerminalWaker,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let ops::StartedAcpSession {
            session,
            acp,
            events,
            approval,
            record_error,
        } = started;
        let workspace_id = session.workspace_id;
        tracing::info!(
            workspace = %workspace_id,
            session = %session.id,
            program = %acp.program().display(),
            pid = acp.pid(),
            acp_session = acp.session_id(),
            "structured session started"
        );
        if let Some(err) = record_error {
            self.report(&err, window, cx);
        }
        if !self.keep_started(workspace_id, &session, window, cx) {
            acp.kill();
            return;
        }
        let chat = ops::SavedChat {
            agent: acp.agent_id().to_owned(),
            session: acp.session_id().to_owned(),
        };
        self.live.saved_chats.insert(workspace_id, chat.clone());
        self.persist(window, cx, move |store| {
            Ok(store.set_setting(&setting_keys::chat(workspace_id), &chat)?)
        });
        let root = self
            .work
            .projects
            .workspace(workspace_id)
            .map(|ws| ws.worktree_path.clone())
            .unwrap_or_default();
        let view = {
            let acp = Arc::clone(&acp);
            let name = agent_name.clone();
            cx.new(|cx| ChatView::new(acp, events, waker, name, root, window, cx))
        };
        view.update(cx, |chat, _| chat.set_approval(approval));
        if acp.resumed() {
            view.update(cx, |chat, _| {
                chat.add_notice(false, "Resumed the previous conversation")
            });
        } else if let Some(reason) = acp.resume_error() {
            let text =
                format!("Could not resume the previous conversation ({reason}); this is a new one");
            view.update(cx, |chat, _| chat.add_notice(true, text));
        }
        let agent_id = acp.agent_id().to_owned();
        let exit = cx.subscribe_in(
            &view,
            window,
            move |this, view, event: &ChatEvent, window, cx| match event {
                ChatEvent::StateChanged => {
                    if let Some(entry) = this.live.diffs.get(&workspace_id) {
                        let diff = entry.diff.clone();
                        view.update(cx, |chat, cx| chat.set_diff(diff, cx));
                    }
                    cx.notify()
                }
                ChatEvent::Exited { code, stderr_tail } => {
                    if !stderr_tail.is_empty() {
                        tracing::info!(workspace = %workspace_id, "agent stderr: {stderr_tail}");
                    }
                    this.on_session_exit(workspace_id, ops::acp_exit_status(*code), window, cx);
                }
                ChatEvent::OpenFile(path) => this.open_in_files(workspace_id, path, window, cx),
                ChatEvent::ConfigChosen { config_id, value } => {
                    let (agent, id, value) = (agent_id.clone(), config_id.clone(), value.clone());
                    this.persist(window, cx, move |store| {
                        ops::remember_agent_config(store, &agent, &id, &value)
                    });
                }
                ChatEvent::ModeChosen(mode) => {
                    let (agent, mode) = (agent_id.clone(), mode.clone());
                    this.persist(window, cx, move |store| {
                        ops::remember_workspace_mode(store, workspace_id, &agent, &mode)
                    });
                }
            },
        );
        let live = LiveSession {
            session,
            agent_name,
            surface: AgentSurface::Chat { acp, view },
            activity: Default::default(),
            _exit: exit,
        };
        self.insert_live_session(workspace_id, live, window, cx);
    }

    /// The saved conversation of `workspace_id` if `agent_id` ran it.
    pub(super) fn resumable_chat(
        &self,
        workspace_id: WorkspaceId,
        agent_id: &str,
    ) -> Option<String> {
        self.live
            .saved_chats
            .get(&workspace_id)
            .filter(|chat| chat.agent == agent_id)
            .map(|chat| chat.session.clone())
    }

    fn forget_chat(&mut self, workspace_id: WorkspaceId) {
        self.live.saved_chats.remove(&workspace_id);
        if let Some(store) = &self.store
            && let Err(err) = store.delete_setting(&setting_keys::chat(workspace_id))
        {
            tracing::warn!(%err, "failed to forget the saved conversation");
        }
    }

    /// Shows `path` (absolute, inside the workspace's worktree) in the Files tab.
    fn open_in_files(
        &mut self,
        workspace_id: WorkspaceId,
        path: &std::path::Path,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(ws) = self.work.projects.workspace(workspace_id) else {
            return;
        };
        let Ok(relative) = path.strip_prefix(&ws.worktree_path) else {
            tracing::info!("not opening {}: outside the worktree", path.display());
            return;
        };
        let relative = relative.to_owned();
        if let Some(pane) = self.layout.panes.pane_of(workspace_id) {
            self.focus_pane(pane, window, cx);
        }
        self.set_files_open(true, window, cx);
        self.files_panel
            .update(cx, |panel, cx| panel.open(relative, cx));
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
        if !self.keep_started(workspace_id, &session, window, cx) {
            if let Err(err) = handle.session().kill() {
                tracing::warn!(%err, "failed to kill agent of a removed workspace");
            }
            return;
        }
        let terminal = Arc::clone(handle.session());
        let options = TerminalViewOptions::for_program(handle.program());
        let view = cx.new(|cx| TerminalView::attach(terminal, waker, options, cx));
        let exit = cx.subscribe_in(
            &view,
            window,
            move |this, _, event: &TerminalEvent, window, cx| match event {
                TerminalEvent::Exited(exit) => {
                    this.on_session_exit(workspace_id, ops::exit_status(exit), window, cx)
                }
            },
        );
        let live = LiveSession {
            session,
            agent_name,
            surface: AgentSurface::Terminal { handle, view },
            activity: Default::default(),
            _exit: exit,
        };
        self.insert_live_session(workspace_id, live, window, cx);
    }

    pub(super) fn on_session_exit(
        &mut self,
        workspace_id: WorkspaceId,
        status: SessionStatus,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(live) = self.live.sessions.get_mut(&workspace_id) else {
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

    /// Kills the workspace's agent, if any, and forgets its view. Returns the session to record
    /// as stopped and a way to wait for the process.
    pub(super) fn close_session(
        &mut self,
        workspace_id: WorkspaceId,
    ) -> (Option<hangar_core::SessionId>, Option<ExitWaiter>) {
        match self.live.sessions.remove(&workspace_id) {
            Some(live) => (live.stop(), Some(live.exit_waiter())),
            None => (None, None),
        }
    }

    pub(super) fn archive_workspace(
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
        self.sync_diffs(cx);
        self.sync_device_focus(window, cx);
        let device = self
            .device_pool
            .update(cx, |pool, cx| pool.archive(workspace_id, cx));
        cx.notify();
        cx.spawn_in(window, async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn(async move { ops::archive_workspace(&store, workspace_id, session) })
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
                    Ok(()) => tracing::info!(workspace = %workspace_id, "workspace archived"),
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

    pub(super) fn restore_workspace(
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

    pub(super) fn confirm_delete_workspace(
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
        if !self.work.busy.insert(workspace_id) {
            return Task::ready(false);
        }
        let (session, terminal) = self.close_session(workspace_id);
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
                        ops::stop_session(&store, session, "workspace deleted")?;
                    }
                    ops::delete_workspace(&store, workspace_id, options)
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
                        this.forget_run_config(workspace_id);
                        this.forget_chat(workspace_id);
                        let device = this
                            .device_pool
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

    pub(super) fn confirm_remove_project(
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
                        let removed = this.work.projects.remove_project(project_id);
                        let mut cleared = false;
                        for ws in removed.iter().flat_map(|e| &e.workspaces) {
                            this.close_session(ws.id);
                            cleared |= this.layout.panes.clear(ws.id);
                        }
                        if cleared {
                            this.save_layout(window, cx);
                        }
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

    pub(super) fn select_workspace(
        &mut self,
        workspace_id: WorkspaceId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.layout.panes.show(workspace_id);
        self.focus_terminal(window, cx);
        self.panes_changed(window, cx);
        cx.notify();
    }

    pub(super) fn focus_pane(&mut self, pane: usize, window: &mut Window, cx: &mut Context<Self>) {
        if self.layout.panes.focused() != pane {
            self.layout.panes.focus(pane);
            self.focus_terminal(window, cx);
            self.panes_changed(window, cx);
            cx.notify();
        }
    }

    pub(super) fn focus_terminal(&self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(id) = self.layout.panes.slot(self.layout.panes.focused()) else {
            return;
        };
        let handle = match self.run_view(id, self.pane_tab(id)) {
            Some(view) => Some(view.focus_handle(cx)),
            None => self
                .live
                .sessions
                .get(&id)
                .map(|live| live.focus_handle(cx)),
        };
        if let Some(handle) = handle {
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
        self.live.pane_tabs.insert(id, tab);
        if let Some(pane) = self.layout.panes.pane_of(id) {
            self.focus_pane(pane, window, cx);
        }
        self.focus_terminal(window, cx);
        cx.notify();
    }

    /// The first active workspace with this slug, for dev hooks.
    pub(super) fn workspace_by_slug(&self, slug: &str) -> Option<WorkspaceId> {
        self.work
            .projects
            .entries()
            .iter()
            .flat_map(|e| &e.workspaces)
            .find(|w| w.slug == slug)
            .map(|w| w.id)
    }
}

/// An agent started off the main thread, before it is attached to a view.
enum Started {
    Terminal(StartedSession),
    Chat(ops::StartedAcpSession),
}
