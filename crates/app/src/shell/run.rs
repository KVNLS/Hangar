//! Run loop (⌘R): build a workspace and run it on its own device (from the device pool). The
//! commands come from `hangar_runner` and run in terminal tabs next to the agent (a dev server
//! tab for Metro, a build tab); installs and launches go through the device.

use std::path::{Path, PathBuf};

use anyhow::Context as _;
use hangar_agents::{AgentAdapter as _, PtyAdapter, SpawnContext};
use hangar_core::SessionStatus;
use hangar_runner::{RunPlan, Stack, Step};
use serde::{Deserialize, Serialize};
use tokio::sync::oneshot;

use super::*;
use crate::device_pool::Lease;
use crate::device_view::DeviceHandle;
use crate::setting_keys;

/// Expo prints this once the app is open on the device and the dev server is attached.
const EXPO_LAUNCHED: &str = "Logs for your project will appear below";
/// Lines of build output quoted when a build fails.
const FAILURE_LINES: usize = 25;

/// Who waits for a run to end, and the device lease it holds until then.
struct Completion {
    done: oneshot::Sender<anyhow::Result<String>>,
    _lease: Lease,
}

/// Which terminal a pane shows for its workspace.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(super) enum PaneTab {
    #[default]
    Agent,
    Server,
    Build,
}

/// Saved per workspace under [`run_key`].
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RunConfig {
    port: Option<u16>,
    scheme: Option<String>,
}

pub(super) struct RunTab {
    pub(super) title: SharedString,
    /// Owns the process; dropping it kills the command.
    handle: AgentHandle,
    pub(super) view: Entity<TerminalView>,
    /// `None` while the command runs.
    pub(super) exit: Option<SessionStatus>,
    _exit: Subscription,
}

impl RunTab {
    fn running(&self) -> bool {
        self.exit.is_none()
    }

    fn kill(&self) {
        if self.running()
            && let Err(err) = self.handle.session().kill()
        {
            tracing::warn!(%err, title = %self.title, "failed to kill run command");
        }
    }

    pub(super) fn state(&self) -> AgentState {
        match &self.exit {
            None => AgentState::Working,
            Some(SessionStatus::Exited { code: 0 }) => AgentState::Review,
            Some(_) => AgentState::Failed,
        }
    }
}

/// The run of one workspace: its tabs and what to do once the build succeeds.
pub(super) struct WorkspaceRun {
    pub(super) stack: &'static str,
    pub(super) port: u16,
    /// The build tab keeps running as the dev server (`expo run`).
    build_serves: bool,
    pub(super) server: Option<RunTab>,
    pub(super) build: Option<RunTab>,
    root: PathBuf,
    env: Vec<(String, String)>,
    /// Server and build waiting for the dependency install in the build tab.
    pending: Option<(Option<hangar_runner::Command>, hangar_runner::Command)>,
    after: Vec<Step>,
    device: SharedString,
    handle: Option<DeviceHandle>,
    /// Install and launch in flight or done.
    pub(super) phase: AfterPhase,
    /// Resolved (and the lease released) once the app is launched or the run failed.
    completion: Option<Completion>,
    /// Watches the build tab for Expo's "launched" line.
    _watch: Option<Task<()>>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum AfterPhase {
    Pending,
    Installing,
    Launched,
    Failed,
}

impl WorkspaceRun {
    fn stop(&self) {
        for tab in self.server.iter().chain(&self.build) {
            tab.kill();
        }
    }

    /// Status bar text and color role.
    pub(super) fn summary(&self) -> (String, AgentState) {
        let build = self.build.as_ref();
        match (build.map(RunTab::state), &self.phase) {
            (Some(AgentState::Working), _) if self.pending.is_some() => {
                ("Installing dependencies".to_owned(), AgentState::Working)
            }
            (Some(AgentState::Failed), _) if self.pending.is_some() => {
                ("Install failed".to_owned(), AgentState::Failed)
            }
            (Some(AgentState::Working), _) if self.build_serves => (
                format!("{} :{}", self.stack, self.port),
                AgentState::Working,
            ),
            (Some(AgentState::Working), _) => {
                (format!("Building · {}", self.stack), AgentState::Working)
            }
            (Some(AgentState::Failed), _) => ("Build failed".to_owned(), AgentState::Failed),
            (_, AfterPhase::Installing) => ("Installing".to_owned(), AgentState::Working),
            (_, AfterPhase::Failed) => ("Install failed".to_owned(), AgentState::Failed),
            _ if self.server.as_ref().is_some_and(RunTab::running) => {
                (format!("Metro :{}", self.port), AgentState::Review)
            }
            _ => (format!("Ran on {}", self.device), AgentState::Review),
        }
    }
}

/// Everything worked out off the main thread before the tabs start.
struct Prepared {
    plan: RunPlan,
    setup: Option<hangar_runner::Command>,
    stack: &'static str,
    port: u16,
    scheme: Option<String>,
    target: hangar_runner::Target,
    adb: Option<PathBuf>,
    env: Vec<(String, String)>,
}

fn prepare(
    root: &Path,
    build_dir: &Path,
    mut target: hangar_runner::Target,
    saved: RunConfig,
    taken: &[u16],
    path: Option<String>,
) -> anyhow::Result<Prepared> {
    let stacks = hangar_runner::detect(root);
    let stack = hangar_runner::pick_stack(&stacks, target.platform).ok_or_else(|| {
        anyhow!(
            "no app to run on {} in {} (looked for Expo, React Native, Xcode and Gradle projects)",
            target.platform.label(),
            root.display()
        )
    })?;
    let scheme = match stack {
        Stack::Xcode { container } => {
            let schemes = hangar_runner::xcode_schemes(root, container)?;
            Some(
                hangar_runner::pick_scheme(container, &schemes, saved.scheme.as_deref())
                    .with_context(|| format!("schemes: {}", schemes.join(", ")))?,
            )
        }
        _ => None,
    };
    let port = match saved.port.filter(|p| !taken.contains(p)) {
        Some(port) => port,
        None => hangar_runner::allocate_port(taken)?,
    };
    // The serial of a running emulator changes per boot: look it up by AVD when not known.
    if let Some(avd) = &target.avd
        && !target.id.starts_with("emulator-")
        && let Some(found) = hangar_devices::android::discovery::scan()
            .into_iter()
            .find(|d| &d.avd_id == avd)
    {
        target.id = found.serial();
    }
    let plan = hangar_runner::plan(&hangar_runner::PlanInput {
        stack,
        target: &target,
        port,
        scheme: scheme.as_deref(),
        build_dir,
    })?;
    let setup = hangar_runner::setup(stack, root);
    let sdk = hangar_devices::android::AndroidSdk::locate().ok();
    let mut path_dirs: Vec<String> = Vec::new();
    let mut env = Vec::new();
    if let Some(sdk) = &sdk {
        for dir in ["platform-tools", "emulator"] {
            path_dirs.push(sdk.root.join(dir).display().to_string());
        }
        env.push(("ANDROID_HOME".to_owned(), sdk.root.display().to_string()));
    }
    path_dirs.extend(path.or_else(|| std::env::var("PATH").ok()));
    env.push(("PATH".to_owned(), path_dirs.join(":")));
    Ok(Prepared {
        plan,
        setup,
        stack: stack.label(),
        port,
        scheme,
        target,
        adb: sdk.map(|s| s.adb),
        env,
    })
}

/// Runs `command` through the user's shell (so `npx` and friends resolve as in a terminal).
fn spawn_command(
    command: &hangar_runner::Command,
    root: &Path,
    env: &[(String, String)],
    hook: hangar_agents::UpdateHook,
) -> anyhow::Result<AgentHandle> {
    let shell = std::env::var("SHELL").unwrap_or_else(|_| "/bin/zsh".to_owned());
    let line = command.shell_line();
    let adapter = PtyAdapter::new("run", "Run", shell).with_args(["-c", line.as_str()]);
    let mut ctx = SpawnContext::new(root.join(&command.cwd));
    ctx.env.extend(env.iter().cloned());
    ctx.env.extend(command.env.iter().cloned());
    tracing::info!(%line, cwd = %ctx.cwd.display(), "run command");
    adapter
        .launch(&ctx)
        .map_err(anyhow::Error::new)
        .and_then(|launch| Ok(crate::pty::spawn(launch, hook)?))
        .with_context(|| format!("failed to start `{line}`"))
}

impl HangarApp {
    /// ⌘R: build and run the focused workspace on its device.
    pub(super) fn run_focused(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some((_, workspace)) = self.focused_workspace() else {
            window.push_notification("Focus a workspace to run it.", cx);
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
        let taken: Vec<u16> = self
            .live
            .runs
            .iter()
            .filter(|(id, _)| **id != workspace.id)
            .map(|(_, run)| run.port)
            .collect();
        let path = self.agent_setup.path.as_ref().map(|p| p.value.clone());
        let root = workspace.worktree_path.clone();
        tracing::info!(workspace = %id, "run requested");
        cx.notify();
        let (done_tx, done_rx) = oneshot::channel();
        cx.spawn_in(window, async move |this, cx| {
            let fail = |this: &WeakEntity<Self>, err: anyhow::Error, cx: &mut AsyncWindowContext| {
                let _ = this.update_in(cx, |this, window, cx| {
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
                let _ = this.update_in(cx, |_, window, cx| {
                    window.push_notification(Notification::info(message), cx);
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
                    async move { prepare(&root, &build_dir, target, saved, &taken, path) }
                })
                .await;
            let prepared = match prepared {
                Ok(p) => p,
                Err(err) => {
                    tracing::warn!(%err, "run: prepare failed");
                    drop(completion);
                    return fail(&this, err, cx);
                }
            };
            if let Some(store) = &store {
                let config = RunConfig {
                    port: Some(prepared.port),
                    scheme: prepared.scheme.clone(),
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
                this.start_run(id, root, prepared, name, handle, completion, window, cx)
            });
            done_rx
                .await
                .unwrap_or_else(|_| Err(anyhow!("the run was dropped")))
        })
    }

    /// Resolves the run's completion (releasing its device lease).
    fn finish_run(&mut self, id: WorkspaceId, result: anyhow::Result<String>) {
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

    #[allow(clippy::too_many_arguments)]
    fn start_run(
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
    fn launch_tabs(&mut self, id: WorkspaceId, window: &mut Window, cx: &mut Context<Self>) {
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
    fn watch_launch(
        &mut self,
        id: WorkspaceId,
        view: Entity<TerminalView>,
        cx: &mut Context<Self>,
    ) -> Task<()> {
        cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(Duration::from_secs(1)).await;
                let done = this.update(cx, |this, cx| {
                    let launched = terminal_lines(&view, cx)
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
    fn spawn_tab(
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
        let view =
            cx.new(|cx| TerminalView::attach(terminal, waker, TerminalViewOptions::default(), cx));
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

    fn on_run_exit(
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
    fn after_build(&mut self, id: WorkspaceId, window: &mut Window, cx: &mut Context<Self>) {
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
                        window.push_notification(Notification::success(summary.clone()), cx);
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
    pub(super) fn stop_run(&mut self, cx: &mut Context<Self>) {
        if let Some(id) = self.layout.panes.slot(self.layout.panes.focused())
            && let Some(run) = self.live.runs.get(&id)
        {
            run.stop();
            cx.notify();
        }
    }

    /// Restarts the focused workspace's dev server on its port.
    pub(super) fn restart_server(&mut self, window: &mut Window, cx: &mut Context<Self>) {
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
    pub(super) fn close_run(&mut self, id: WorkspaceId) {
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
    pub(super) fn forget_run_config(&self, id: WorkspaceId) {
        if let Some(store) = &self.store
            && let Err(err) = store.delete_setting(&setting_keys::run(id))
        {
            tracing::warn!(%err, "failed to forget the run config");
        }
    }

    /// The tab a pane shows for `id`, falling back to the agent when that run tab is gone.
    pub(super) fn pane_tab(&self, id: WorkspaceId) -> PaneTab {
        let tab = self.live.pane_tabs.get(&id).copied().unwrap_or_default();
        let run = self.live.runs.get(&id);
        let exists = match tab {
            PaneTab::Agent => true,
            PaneTab::Server => run.is_some_and(|r| r.server.is_some()),
            PaneTab::Build => run.is_some_and(|r| r.build.is_some()),
        };
        if exists { tab } else { PaneTab::Agent }
    }

    pub(super) fn run_view(&self, id: WorkspaceId, tab: PaneTab) -> Option<Entity<TerminalView>> {
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
