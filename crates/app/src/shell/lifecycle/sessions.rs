//! Starting, attaching and ending agent sessions.

use super::*;

impl HangarApp {
    /// Starts `agent_id` in the workspace, or queues it at "Max concurrent agents". With
    /// `resume`, a structured agent continues the workspace's saved conversation when it ran
    /// with the same preset. Resolves to whether it started now.
    pub(in crate::shell) fn start_session(
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
    pub(in crate::shell) fn launch_session(
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
    pub(in crate::shell) fn keep_started(
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

    pub(in crate::shell) fn insert_live_session(
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

    pub(in crate::shell) fn attach_session(
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
        let options = self.terminal_options(handle.program());
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

    pub(in crate::shell) fn on_session_exit(
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
    pub(in crate::shell) fn close_session(
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
}

/// An agent started off the main thread, before it is attached to a view.
enum Started {
    Terminal(StartedSession),
    Chat(ops::StartedAcpSession),
}
