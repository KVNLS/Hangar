//! What happens when run commands end, and stopping or closing a run.

use super::*;

impl HangarApp {
    pub(super) fn on_run_exit(
        &mut self,
        id: WorkspaceId,
        kind: PaneTab,
        exit: &Result<ExitInfo, String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let status = ops::exit_status(exit);
        let Some(run) = self.live.runs.get_mut(&id) else {
            return;
        };
        let tab = match kind {
            PaneTab::Server => run.server.as_mut(),
            PaneTab::Build => run.build.as_mut(),
            PaneTab::Agent => None,
        };
        let Some(tab) = tab else {
            return;
        };
        tracing::info!(workspace = %id, title = %tab.title, ?status, "run command ended");
        tab.exit = Some(status.clone());
        let title = tab.title.clone();
        let view = tab.view.clone();
        cx.notify();
        if kind != PaneTab::Build {
            return;
        }
        if status == (SessionStatus::Exited { code: 0 }) {
            if run.pending.is_some() {
                self.launch_tabs(id, window, cx);
            } else if run
                .after
                .iter()
                .any(|s| matches!(s, Step::InstallAndLaunch(_)))
            {
                self.after_build(id, window, cx);
            } else {
                // The JS CLIs install and launch themselves.
                let summary = format!("{} launched the app on {}", run.stack, run.device);
                self.finish_run(id, Ok(summary));
            }
        } else {
            let tail = terminal_lines(&view, cx);
            let tail = tail[tail.len().saturating_sub(FAILURE_LINES)..].join("\n");
            let what = match status {
                SessionStatus::Exited { code } => format!("exit {code}"),
                SessionStatus::Failed { message } => message,
                other => format!("{other:?}"),
            };
            self.finish_run(id, Err(anyhow!("{title} failed ({what}):\n{tail}")));
        }
    }

    /// Installs and launches what a native build produced.
    pub(super) fn after_build(
        &mut self,
        id: WorkspaceId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(run) = self.live.runs.get_mut(&id) else {
            return;
        };
        let artifacts: Vec<hangar_runner::Artifact> = run
            .after
            .iter()
            .filter_map(|step| match step {
                Step::InstallAndLaunch(a) => Some(a.clone()),
                Step::AdbReverse(_) => None,
            })
            .collect();
        if artifacts.is_empty() {
            return;
        }
        let Some(handle) = run.handle.clone() else {
            self.finish_run(
                id,
                Err(anyhow!("the device was released before the install")),
            );
            return;
        };
        run.phase = AfterPhase::Installing;
        let root = run.root.clone();
        let device = run.device.clone();
        cx.notify();
        cx.spawn_in(window, async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn(async move {
                    let mut launched = Vec::new();
                    for artifact in &artifacts {
                        let built = hangar_runner::resolve_artifact(&root, artifact)?;
                        handle.install(&built.path).with_context(|| {
                            format!("failed to install {}", built.path.display())
                        })?;
                        handle
                            .launch(&built.app_id)
                            .with_context(|| format!("failed to launch {}", built.app_id))?;
                        launched.push(built.app_id);
                    }
                    anyhow::Ok(launched)
                })
                .await;
            let _ = this.update_in(cx, |this, window, cx| {
                let ok = result.is_ok();
                if let Some(run) = this.live.runs.get_mut(&id) {
                    run.phase = if ok {
                        AfterPhase::Launched
                    } else {
                        AfterPhase::Failed
                    };
                }
                match result {
                    Ok(apps) => {
                        tracing::info!(?apps, %device, "run: launched");
                        let summary = format!("Launched {} on {device}", apps.join(", "));
                        this.show_toast(ToastKind::Success, summary.clone(), cx);
                        this.finish_run(id, Ok(summary));
                    }
                    Err(err) => {
                        tracing::warn!(%err, "run: install/launch failed");
                        this.report(&err, window, cx);
                        this.finish_run(id, Err(err));
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// ⌘.: stops the focused workspace's build and dev server.
    pub(in crate::shell) fn stop_run(&mut self, cx: &mut Context<Self>) {
        if let Some(id) = self.layout.panes.slot(self.layout.panes.focused())
            && let Some(run) = self.live.runs.get(&id)
        {
            run.stop();
            cx.notify();
        }
    }

    /// Restarts the focused workspace's dev server on its port.
    pub(in crate::shell) fn restart_server(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(id) = self.layout.panes.slot(self.layout.panes.focused()) else {
            return;
        };
        if let Some(server) = self
            .live
            .runs
            .get_mut(&id)
            .and_then(|run| run.server.take())
        {
            server.kill();
        }
        self.run_focused(window, cx);
    }

    /// Kills the workspace's run commands and frees its port (archive, delete).
    pub(in crate::shell) fn close_run(&mut self, id: WorkspaceId) {
        if let Some(mut run) = self.live.runs.remove(&id) {
            run.stop();
            if let Some(completion) = run.completion.take() {
                let _ = completion
                    .done
                    .send(Err(anyhow!("the workspace was closed during the run")));
            }
        }
        self.live.pane_tabs.remove(&id);
    }

    /// Drops the saved port so another workspace can take it (delete).
    pub(in crate::shell) fn forget_run_config(&self, id: WorkspaceId) {
        if let Some(store) = &self.store
            && let Err(err) = store.delete_setting(&setting_keys::run(id))
        {
            tracing::warn!(%err, "failed to forget the run config");
        }
    }

    /// The tab a pane shows for `id`, falling back to the agent when that run tab is gone.
    pub(in crate::shell) fn pane_tab(&self, id: WorkspaceId) -> PaneTab {
        let tab = self.live.pane_tabs.get(&id).copied().unwrap_or_default();
        let run = self.live.runs.get(&id);
        let exists = match tab {
            PaneTab::Agent => true,
            PaneTab::Server => run.is_some_and(|r| r.server.is_some()),
            PaneTab::Build => run.is_some_and(|r| r.build.is_some()),
        };
        if exists { tab } else { PaneTab::Agent }
    }

    pub(in crate::shell) fn run_view(
        &self,
        id: WorkspaceId,
        tab: PaneTab,
    ) -> Option<Entity<TerminalView>> {
        let run = self.live.runs.get(&id)?;
        match tab {
            PaneTab::Server => run.server.as_ref().map(|t| t.view.clone()),
            PaneTab::Build => run.build.as_ref().map(|t| t.view.clone()),
            PaneTab::Agent => None,
        }
    }
}

/// The lines on a terminal's screen, trailing blank lines dropped.
fn terminal_lines(view: &Entity<TerminalView>, cx: &App) -> Vec<String> {
    let snapshot = view.read(cx).session().terminal().snapshot();
    let mut lines: Vec<String> = (0..snapshot.rows)
        .map(|r| snapshot.row_text(r).trim_end().to_owned())
        .collect();
    while lines.last().is_some_and(String::is_empty) {
        lines.pop();
    }
    lines
}
