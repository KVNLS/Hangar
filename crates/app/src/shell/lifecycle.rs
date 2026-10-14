//! Workspace and session lifecycle: create, start, attach, exit, archive, restore, delete.

use std::sync::{Mutex, PoisonError};

use super::*;

/// Held while a worktree and its workspace row are created.
static CREATE_LOCK: Mutex<()> = Mutex::new(());

/// A workspace just created, and its agent's start.
pub(super) struct SpawnedWorkspace {
    pub workspace: Workspace,
    /// How many starts wait before its agent's, when it was queued.
    pub queued: Option<usize>,
    /// Resolves to whether the agent started now.
    pub start: Task<bool>,
}

impl HangarApp {
    /// Creates the worktree and workspace off the main thread, assigns it `target` as its base
    /// device (else the default on first use), shows it in the focused pane and starts
    /// `agent_id` in it. Resolves once the agent has started, failed or been queued. The task
    /// of `new` is typed into the agent once it runs, which may be later if the start is queued.
    pub(super) fn create_workspace(
        &mut self,
        project_id: ProjectId,
        new: ops::NewWorkspace,
        agent_id: String,
        target: Option<crate::device_panel::DeviceKey>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Task<Option<WorkspaceId>> {
        let created = self.spawn_workspace(project_id, new, agent_id, target, true, window, cx);
        cx.spawn_in(window, async move |this, cx| match created.await {
            Ok(spawned) => {
                spawned.start.await;
                Some(spawned.workspace.id)
            }
            Err(err) => {
                let _ = this.update_in(cx, |this, window, cx| this.report(&err, window, cx));
                None
            }
        })
    }

    /// [`Self::create_workspace`] without reporting errors, and showing the new workspace in
    /// the focused pane only with `show`. Resolves once the workspace exists and its agent was
    /// asked to start.
    #[allow(clippy::too_many_arguments)]
    pub(super) fn spawn_workspace(
        &mut self,
        project_id: ProjectId,
        new: ops::NewWorkspace,
        agent_id: String,
        target: Option<crate::device_panel::DeviceKey>,
        show: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Task<anyhow::Result<SpawnedWorkspace>> {
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
            Err(err) => return Task::ready(Err(err)),
        };
        self.work.creating.insert(project_id);
        cx.notify();

        let input = new.task.clone().filter(|t| !t.trim().is_empty());
        cx.spawn_in(window, async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn(async move {
                    // Agents may create several workspaces at once; git worktree commands on one
                    // repository must not race.
                    let _serial = CREATE_LOCK.lock().unwrap_or_else(PoisonError::into_inner);
                    ops::create_workspace_from(&store, &project, &root, &new)
                })
                .await;
            this.update_in(cx, |this, window, cx| {
                this.work.creating.remove(&project_id);
                cx.notify();
                let workspace = result?;
                tracing::info!(
                    name = %workspace.name,
                    branch = %workspace.branch,
                    path = %workspace.worktree_path.display(),
                    "workspace created"
                );
                let id = workspace.id;
                this.work.projects.upsert_workspace(workspace.clone());
                this.track_workspace(id, cx);
                if let Some(target) = target {
                    this.device_pool.update(cx, |pool, cx| {
                        pool.assign(id, target.platform, target.id, cx)
                    });
                }
                if show {
                    this.layout.panes.show(id);
                    this.panes_changed(window, cx);
                }
                let request = queue::StartRequest {
                    agent: agent_id,
                    resume: false,
                    input,
                };
                let start = this.request_agent(id, request, window, cx);
                Ok(SpawnedWorkspace {
                    queued: this.queued_ahead(id),
                    workspace,
                    start,
                })
            })
            .map_err(|_| anyhow!("the window closed"))?
        })
    }

    /// Starts `agent_id` in the workspace, or queues it at "Max concurrent agents". With
    /// `resume`, a structured agent continues the workspace's saved conversation when it ran
    /// with the same preset. Resolves to whether it started now.
    pub(super) fn start_session(
        &mut self,
        workspace_id: WorkspaceId,
        agent_id: String,
        resume: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Task<bool> {
        let request = queue::StartRequest {
            agent: agent_id,
            resume,
            input: None,
        };
        self.request_agent(workspace_id, request, window, cx)
    }

    /// Spawns the agent off the main thread, past the queue, and attaches a terminal (or chat)
    /// view to it, then types the request's input. Resolves to whether it started.
    pub(super) fn launch_session(
        &mut self,
        workspace_id: WorkspaceId,
        request: queue::StartRequest,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Task<bool> {
        let queue::StartRequest {
            agent: agent_id,
            resume,
            input,
        } = request;
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
        self.remember_workspace_agent(workspace_id, &agent_id, window, cx);
        let path = self.agent_setup.path.as_ref().map(|p| p.value.clone());
        if let Some(message) = self.ollama_unavailable(&agent_id) {
            window.push_notification(Notification::warning(message), cx);
        }
        // Without the device tools the agent still runs: say why, don't block it.
        let mut extras = self
            .agent_extras(adapter.id(), &workspace, cx)
            .unwrap_or_else(|err| {
                tracing::warn!("agent starts without device tools: {err:#}");
                Default::default()
            });
        extras.prompt = input.clone();
        let waker = TerminalWaker::new();
        let hook = waker.hook();
        let agent_name: SharedString = adapter.display_name().to_owned().into();
        let resume_id = resume
            .then(|| self.resumable_chat(workspace_id, &agent_id))
            .flatten();
        self.live.starting.insert(workspace_id);

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
            this.update_in(cx, |this, window, cx| {
                let ok = match started {
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
                };
                this.live.starting.remove(&workspace_id);
                if let Some(input) = input.filter(|_| ok)
                    && let Some(live) = this.live.sessions.get(&workspace_id)
                    && let Err(err) = live.send_text(&input, cx)
                {
                    tracing::error!("could not send the task to the agent: {err:#}");
                }
                // A failed start, or one dropped by `keep_started`, frees its slot.
                this.drain_agent_queue(window, cx);
                ok
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
        self.record_session_end(window, cx, move |store| {
            ops::stop_session(store, id, "workspace closed while starting", None)
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
            let changes = self.diff_stat(workspace_id, cx);
            self.record_session_end(window, cx, move |store| {
                ops::stop_session(store, id, "replaced by a new session", changes)
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
                    let cost = view.read(cx).usage().cost();
                    this.save_session_cost(workspace_id, cost, window, cx);
                    let spend = view.update(cx, |chat, _| chat.take_spend());
                    this.book_spend(workspace_id, spend, window, cx);
                    cx.notify()
                }
                ChatEvent::Exited { code, stderr_tail } => {
                    let spend = view.update(cx, |chat, _| chat.take_spend());
                    this.book_spend(workspace_id, spend, window, cx);
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
            signal: None,
            transcript_path: None,
            saved_cost: None,
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
        self.live.workspace_agents.remove(&workspace_id);
        if let Some(store) = &self.store {
            if let Err(err) = store.delete_setting(&setting_keys::chat(workspace_id)) {
                tracing::warn!(%err, "failed to forget the saved conversation");
            }
            if let Err(err) = store.delete_setting(&setting_keys::workspace_agent(workspace_id)) {
                tracing::warn!(%err, "failed to forget the workspace's agent");
            }
        }
    }

    /// Saves what the workspace's session has cost when the agent reported a new total, so the
    /// day's total survives a relaunch.
    fn save_session_cost(
        &mut self,
        workspace_id: WorkspaceId,
        cost: Option<hangar_agents::acp::protocol::Cost>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(cost) = cost.as_ref().map(crate::chat_view::stored_cost) else {
            return;
        };
        let Some(live) = self.live.sessions.get_mut(&workspace_id) else {
            return;
        };
        if live.saved_cost.as_ref() == Some(&cost) {
            return;
        }
        live.saved_cost = Some(cost.clone());
        let session = live.session.id;
        self.persist(window, cx, move |store| {
            store
                .record_session_cost(session, &cost)
                .map_err(|err| anyhow!("failed to save the session's cost: {err}"))
        });
    }

    /// Adds what the workspace's chat spent to the usage ledger.
    fn book_spend(
        &mut self,
        workspace_id: WorkspaceId,
        spend: Option<crate::chat_view::Spend>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(spend) = spend else {
            return;
        };
        let Some(live) = self.live.sessions.get(&workspace_id) else {
            return;
        };
        let rows = hangar_usecases::usage::book_turn(
            live.session.id,
            hangar_core::Timestamp::now(),
            crate::chat_view::turn_spend(&spend),
            spend.model.as_deref(),
        );
        if rows.is_empty() {
            return;
        }
        self.persist(window, cx, move |store| {
            rows.iter().try_for_each(|row| {
                store
                    .record_usage(row)
                    .map_err(|err| anyhow!("failed to save usage: {err}"))
            })
        });
    }

    /// The agent to start in `workspace_id`: the one last chosen or run there, else the default (Settings › Agents).
    pub(super) fn agent_for(&self, workspace_id: WorkspaceId) -> String {
        self.live
            .workspace_agents
            .get(&workspace_id)
            .filter(|id| agents::preset(id).is_some())
            .cloned()
            .unwrap_or_else(|| self.agent_setup.agent.to_string())
    }

    pub(super) fn remember_workspace_agent(
        &mut self,
        workspace_id: WorkspaceId,
        agent_id: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self
            .live
            .workspace_agents
            .get(&workspace_id)
            .map(String::as_str)
            == Some(agent_id)
        {
            return;
        }
        self.live
            .workspace_agents
            .insert(workspace_id, agent_id.to_owned());
        let agent = agent_id.to_owned();
        self.persist(window, cx, move |store| {
            ops::save_workspace_agent(store, workspace_id, &agent)
        });
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
            signal: None,
            transcript_path: None,
            saved_cost: None,
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
        self.mark_asking(workspace_id, false, cx);
        self.withdraw_question(workspace_id, "the agent exited");
        cx.notify();
        let changes = self.diff_stat(workspace_id, cx);
        self.record_session_end(window, cx, move |store| {
            ops::finish_session(store, session_id, &status, changes).map(drop)
        });
        self.drain_agent_queue(window, cx);
    }

    /// Kills the workspace's agent, if any, and forgets its view; a queued start is dropped.
    /// Returns the session to record as stopped and a way to wait for the process. Callers
    /// drain the queue afterwards: a slot may have freed.
    pub(super) fn close_session(
        &mut self,
        workspace_id: WorkspaceId,
    ) -> (Option<hangar_core::SessionId>, Option<ExitWaiter>) {
        self.live.agent_queue.remove(workspace_id);
        self.withdraw_question(workspace_id, "the workspace's agent was stopped");
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
                            // Its simulator clone and assignment go with it, as on delete.
                            let id = ws.id;
                            let deleted =
                                this.device_pool.update(cx, |pool, cx| pool.delete(id, cx));
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
