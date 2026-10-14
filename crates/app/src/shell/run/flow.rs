//! The run flow: ⌘R, device lease, preparation and the user's app or scheme choice.

use super::*;

impl HangarApp {
    /// ⌘R: build and run the focused workspace on its device.
    pub(in crate::shell) fn run_focused(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some((_, workspace)) = self.focused_workspace() else {
            self.show_toast(ToastKind::Info, "Focus a workspace to run it.", cx);
            return;
        };
        let id = workspace.id;
        // Failures are reported by the run itself.
        self.run_workspace(id, window, cx).detach();
    }

    /// Builds and runs workspace `id` (shown or not) on its device, the same as ⌘R: takes the
    /// device's lease (waiting in line in single-device mode), boots and opens the device if
    /// needed, then builds in the workspace's tabs. Resolves once the app is launched (with a
    /// short summary) or the run failed (with the reason and, for a failed build, its last
    /// lines). Failures are also reported in the window.
    pub fn run_workspace(
        &mut self,
        id: WorkspaceId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Task<anyhow::Result<String>> {
        let Some(workspace) = self
            .work
            .projects
            .workspace(id)
            .filter(|_| self.work.projects.is_active(id))
            .cloned()
        else {
            return Task::ready(Err(anyhow!("unknown or archived workspace")));
        };
        if self.work.busy.contains(&id) {
            return Task::ready(Err(anyhow!(
                "the workspace is being archived, restored or deleted"
            )));
        }
        let Some(build_dir) = paths::builds_root().map(|r| r.join(workspace.id.to_string())) else {
            let err = anyhow!("no build directory (HOME is not set)");
            self.report(&err, window, cx);
            return Task::ready(Err(err));
        };
        self.track_workspace(id, cx);
        let acquire = self.device_pool.update(cx, |pool, cx| pool.acquire(id, cx));
        let store = self.store.clone();
        let saved: RunConfig = store
            .as_ref()
            .and_then(|s| {
                s.get_setting(&setting_keys::run(workspace.id))
                    .ok()
                    .flatten()
            })
            .unwrap_or_default();
        // Picked here, not while preparing, so two runs started together get different ports.
        let taken: Vec<u16> = self
            .live
            .runs
            .iter()
            .map(|(id, run)| (id, run.port))
            .chain(self.live.port_holds.iter().map(|(id, port)| (id, *port)))
            .filter(|(id, _)| **id != workspace.id)
            .map(|(_, port)| port)
            .collect();
        let port = match hangar_runner::pick_port(saved.port, &taken) {
            Ok(port) => port,
            Err(err) => {
                let err = anyhow::Error::from(err);
                self.report(&err, window, cx);
                return Task::ready(Err(err));
            }
        };
        self.live.port_holds.insert(id, port);
        let path = self.agent_setup.path.as_ref().map(|p| p.value.clone());
        let root = workspace.worktree_path.clone();
        tracing::info!(workspace = %id, "run requested");
        cx.notify();
        let (done_tx, done_rx) = oneshot::channel();
        cx.spawn_in(window, async move |this, cx| {
            let fail = |this: &WeakEntity<Self>, err: anyhow::Error, cx: &mut AsyncWindowContext| {
                let _ = this.update_in(cx, |this, window, cx| {
                    this.live.port_holds.remove(&id);
                    this.report(&err, window, cx);
                    cx.notify();
                });
                Err(err)
            };
            let acquired = match acquire.await {
                Ok(acquired) => acquired,
                Err(err) => {
                    tracing::warn!(workspace = %id, "run: no device: {err:#}");
                    return fail(&this, err.context("no device to run on"), cx);
                }
            };
            let info = acquired.session.info().clone();
            let name: SharedString = info.name.clone().into();
            let handle = acquired.session.handle();
            let target = hangar_runner::Target {
                platform: acquired.platform,
                id: info.udid.clone(),
                avd: acquired.avd.clone(),
            };
            tracing::info!(workspace = %id, device = %info.udid, name = %info.name, borrowed = acquired.borrowed, "run");
            if acquired.borrowed {
                let message = format!("Running on {name}, shared with another workspace.");
                let _ = this.update(cx, |this, cx| {
                    this.show_toast(ToastKind::Info, message, cx);
                });
            }
            let completion = Completion {
                done: done_tx,
                _lease: acquired.lease,
            };
            let prepared = cx
                .background_executor()
                .spawn({
                    let root = root.clone();
                    async move { prepare(&root, &build_dir, target, saved, port, path) }
                })
                .await;
            let prepared = match prepared {
                Ok(p) => p,
                Err(err) => {
                    drop(completion);
                    if let Some(choice) = err.downcast_ref::<NeedsChoice>() {
                        let (kind, options) = (choice.kind, choice.options.clone());
                        let _ = this.update_in(cx, |this, window, cx| {
                            this.ask_run_choice(id, kind, options, window, cx)
                        });
                        return Err(err);
                    }
                    tracing::warn!(%err, "run: prepare failed");
                    return fail(&this, err, cx);
                }
            };
            if let Some(store) = &store {
                let config = RunConfig {
                    port: Some(prepared.port),
                    scheme: prepared.scheme.clone(),
                    stack: Some(prepared.stack_key.clone()),
                };
                if let Err(err) = store.set_setting(&setting_keys::run(id), &config) {
                    tracing::warn!(%err, "run: failed to save the run config");
                }
            }
            for step in &prepared.plan.before {
                if let Step::AdbReverse(port) = step {
                    let (adb, serial, port) =
                        (prepared.adb.clone(), prepared.target.id.clone(), *port);
                    let result = cx
                        .background_executor()
                        .spawn(async move {
                            let adb = adb.ok_or_else(|| anyhow!("adb not found (Android SDK)"))?;
                            hangar_devices::android::adb::Adb::new(adb)
                                .reverse(&serial, port)
                                .map_err(anyhow::Error::from)
                        })
                        .await;
                    if let Err(err) = result {
                        tracing::warn!(%err, "run: adb reverse failed");
                    }
                }
            }
            let _ = this.update_in(cx, |this, window, cx| {
                this.live.port_holds.remove(&id);
                this.start_run(id, root, prepared, name, handle, completion, window, cx)
            });
            done_rx
                .await
                .unwrap_or_else(|_| Err(anyhow!("the run was dropped")))
        })
    }

    /// Asks which app (or scheme) workspace `id` runs, saves the answer and runs again.
    pub(super) fn ask_run_choice(
        &mut self,
        id: WorkspaceId,
        kind: ChoiceKind,
        options: Vec<(String, String)>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        /// More buttons than this do not fit a dialog.
        const MAX_OPTIONS: usize = 8;
        let (message, detail) = match kind {
            ChoiceKind::Stack => (
                "Which app should Run start?",
                "This worktree holds several apps for this device. Hangar remembers the choice \
                 for this workspace.",
            ),
            ChoiceKind::Scheme => (
                "Which scheme should Run build?",
                "The Xcode project has several schemes. Hangar remembers the choice for this \
                 workspace.",
            ),
        };
        let shown = &options[..options.len().min(MAX_OPTIONS)];
        let mut buttons: Vec<&str> = shown.iter().map(|(_, label)| label.as_str()).collect();
        buttons.push("Cancel");
        let answer = window.prompt(PromptLevel::Info, message, Some(detail), &buttons, cx);
        let shown = shown.to_vec();
        let store = self.store.clone();
        cx.spawn_in(window, async move |this, cx| {
            let Ok(index) = answer.await else {
                return;
            };
            let Some((value, label)) = shown.into_iter().nth(index) else {
                return;
            };
            tracing::info!(workspace = %id, ?kind, %label, "run choice");
            if let Some(store) = store {
                let key = setting_keys::run(id);
                let mut config: RunConfig =
                    store.get_setting(&key).ok().flatten().unwrap_or_default();
                match kind {
                    ChoiceKind::Stack => {
                        config.stack = Some(value);
                        // Another app has its own schemes.
                        config.scheme = None;
                    }
                    ChoiceKind::Scheme => config.scheme = Some(value),
                }
                if let Err(err) = store.set_setting(&key, &config) {
                    tracing::warn!(%err, "run: failed to save the choice");
                }
            }
            let _ = this.update_in(cx, |this, window, cx| {
                this.run_workspace(id, window, cx).detach();
            });
        })
        .detach();
    }

    /// Resolves the run's completion (releasing its device lease).
    pub(super) fn finish_run(&mut self, id: WorkspaceId, result: anyhow::Result<String>) {
        let Some(completion) = self
            .live
            .runs
            .get_mut(&id)
            .and_then(|r| r.completion.take())
        else {
            return;
        };
        match &result {
            Ok(summary) => tracing::info!(workspace = %id, %summary, "run finished"),
            Err(err) => tracing::info!(workspace = %id, "run failed: {err:#}"),
        }
        let _ = completion.done.send(result);
    }
}
