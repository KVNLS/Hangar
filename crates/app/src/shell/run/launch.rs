//! Starting the install, build and dev-server tabs of a run.

use super::*;

impl HangarApp {
    #[allow(clippy::too_many_arguments)]
    pub(super) fn start_run(
        &mut self,
        id: WorkspaceId,
        root: PathBuf,
        prepared: Prepared,
        device: SharedString,
        handle: DeviceHandle,
        completion: Completion,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self.work.projects.is_active(id) || self.work.busy.contains(&id) {
            let _ = completion
                .done
                .send(Err(anyhow!("the workspace was closed before the build")));
            return;
        }
        let Prepared {
            plan,
            setup,
            stack,
            port,
            env,
            ..
        } = prepared;
        let previous = self.live.runs.remove(&id);
        let (mut server, old_build) = match previous {
            Some(mut run) => {
                if let Some(old) = run.completion.take() {
                    let _ = old.done.send(Err(anyhow!("replaced by a new run")));
                }
                (run.server, run.build)
            }
            None => (None, None),
        };
        if let Some(old) = old_build {
            old.kill();
        }
        // Keep a running dev server unless it serves another port.
        let keep_server = server
            .as_ref()
            .is_some_and(|s| s.running() && s.title.ends_with(&format!(":{port}")));
        if !keep_server && let Some(old) = server.take() {
            old.kill();
        }
        let server_command = if keep_server { None } else { plan.server };
        let mut run = WorkspaceRun {
            stack,
            port,
            build_serves: plan.build_serves,
            server,
            build: None,
            root,
            env,
            pending: Some((server_command, plan.build)),
            after: plan.after,
            device,
            handle: Some(handle),
            phase: AfterPhase::Pending,
            completion: Some(completion),
            _watch: None,
        };
        if let Some(setup) = setup {
            let title = format!("Install · {}", setup.program);
            run.build = self.spawn_tab(
                id,
                PaneTab::Build,
                title,
                &setup,
                &run.root,
                &run.env,
                window,
                cx,
            );
            let started = run.build.is_some();
            self.live.runs.insert(id, run);
            if !started {
                self.finish_run(id, Err(anyhow!("the dependency install did not start")));
            }
        } else {
            self.live.runs.insert(id, run);
            self.launch_tabs(id, window, cx);
        }
        self.live.pane_tabs.insert(id, PaneTab::Build);
        self.focus_terminal(window, cx);
        cx.notify();
    }

    /// Starts the pending dev server and build of `id`.
    pub(super) fn launch_tabs(
        &mut self,
        id: WorkspaceId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(run) = self.live.runs.get_mut(&id) else {
            return;
        };
        let Some((server_command, build_command)) = run.pending.take() else {
            return;
        };
        let (root, env, port, stack) = (run.root.clone(), run.env.clone(), run.port, run.stack);
        let build_title = if run.build_serves {
            format!("{stack} :{port}")
        } else {
            format!("Build · {stack}")
        };
        let server = server_command.and_then(|command| {
            self.spawn_tab(
                id,
                PaneTab::Server,
                format!("Metro :{port}"),
                &command,
                &root,
                &env,
                window,
                cx,
            )
        });
        let build = self.spawn_tab(
            id,
            PaneTab::Build,
            build_title,
            &build_command,
            &root,
            &env,
            window,
            cx,
        );
        let started = build.is_some();
        let watch = build
            .as_ref()
            .filter(|_| self.live.runs.get(&id).is_some_and(|r| r.build_serves))
            .map(|tab| self.watch_launch(id, tab.view.clone(), cx));
        if let Some(run) = self.live.runs.get_mut(&id) {
            if server.is_some() {
                run.server = server;
            }
            run.build = build;
            run._watch = watch;
        }
        if !started {
            self.finish_run(id, Err(anyhow!("the build did not start")));
        }
        self.focus_terminal(window, cx);
        cx.notify();
    }

    /// `expo run` keeps running as the dev server: the run is done once it says the app is open.
    pub(super) fn watch_launch(
        &mut self,
        id: WorkspaceId,
        view: Entity<TerminalView>,
        cx: &mut Context<Self>,
    ) -> Task<()> {
        cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(Duration::from_secs(1)).await;
                let done = this.update(cx, |this, cx| {
                    // The scrollback too: app logs can push the line off screen within the
                    // second between two checks.
                    let launched = view
                        .read(cx)
                        .session()
                        .terminal()
                        .recent_lines(LAUNCH_SCAN_LINES)
                        .iter()
                        .any(|l| l.contains(EXPO_LAUNCHED));
                    if launched {
                        let summary = this.live.runs.get(&id).map(|r| {
                            format!(
                                "{} dev server on :{}, app open on {}",
                                r.stack, r.port, r.device
                            )
                        });
                        this.finish_run(id, Ok(summary.unwrap_or_default()));
                    }
                    launched
                        || this
                            .live
                            .runs
                            .get(&id)
                            .is_none_or(|r| r.completion.is_none())
                });
                if done.unwrap_or(true) {
                    break;
                }
            }
        })
    }

    #[allow(clippy::too_many_arguments)]
    pub(super) fn spawn_tab(
        &mut self,
        id: WorkspaceId,
        kind: PaneTab,
        title: String,
        command: &hangar_runner::Command,
        root: &Path,
        env: &[(String, String)],
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<RunTab> {
        let waker = TerminalWaker::new();
        let handle = match spawn_command(command, root, env, waker.hook()) {
            Ok(handle) => handle,
            Err(err) => {
                self.report(&err, window, cx);
                return None;
            }
        };
        let terminal = Arc::clone(handle.session());
        let options = self.terminal_options(handle.program());
        let view = cx.new(|cx| TerminalView::attach(terminal, waker, options, cx));
        let exit = cx.subscribe_in(
            &view,
            window,
            move |this, _, event: &TerminalEvent, window, cx| match event {
                TerminalEvent::Exited(exit) => this.on_run_exit(id, kind, exit, window, cx),
            },
        );
        Some(RunTab {
            title: title.into(),
            handle,
            view,
            exit: None,
            _exit: exit,
        })
    }
}
