//! Run loop (⌘R): build the focused workspace and run it on the device open in the panel. The
//! commands come from `hangar_runner` and run in terminal tabs next to the agent (a dev server
//! tab for Metro, a build tab); installs and launches go through the device.

use std::path::{Path, PathBuf};

use anyhow::Context as _;
use hangar_agents::{AgentAdapter as _, PtyAdapter, SpawnContext};
use hangar_core::SessionStatus;
use hangar_runner::{RunPlan, Stack, Step};
use serde::{Deserialize, Serialize};

use super::*;
use crate::device_panel::RunTarget;
use crate::device_view::DeviceHandle;

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
struct RunConfig {
    port: Option<u16>,
    scheme: Option<String>,
}

fn run_key(id: WorkspaceId) -> String {
    format!("run:{id}")
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
    pub(super) server: Option<RunTab>,
    pub(super) build: Option<RunTab>,
    root: PathBuf,
    after: Vec<Step>,
    device: SharedString,
    handle: Option<DeviceHandle>,
    /// Install and launch in flight or done.
    pub(super) phase: AfterPhase,
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
    use hangar_devices::Platform as P;
    let stacks = hangar_runner::detect(root);
    let stack = hangar_runner::pick_stack(&stacks, target.platform).ok_or_else(|| {
        anyhow!(
            "no app to run on {} in {} (looked for Expo, React Native, Flutter, Xcode and Gradle projects)",
            match target.platform {
                P::Ios => "iOS",
                P::Android => "Android",
            },
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
    // The serial of a running emulator changes per boot: look it up by AVD now.
    if let Some(avd) = &target.avd
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
    ctx.on_update = Some(hook);
    tracing::info!(%line, cwd = %ctx.cwd.display(), "run command");
    adapter
        .spawn(&ctx)
        .with_context(|| format!("failed to start `{line}`"))
}

impl HangarApp {
    /// ⌘R: build and run the focused workspace on the open device.
    pub(super) fn run_focused(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some((_, workspace)) = self.focused_workspace() else {
            window.push_notification("Focus a workspace to run it.", cx);
            return;
        };
        let workspace = workspace.clone();
        let Some(RunTarget {
            target,
            name,
            handle,
        }) = self.device_panel.read(cx).run_target(cx)
        else {
            window.push_notification("Open a device in the Device tab (⌘1) to run on it.", cx);
            return;
        };
        let Some(build_dir) = paths::builds_root().map(|r| r.join(workspace.id.to_string())) else {
            self.report(&anyhow!("no build directory (HOME is not set)"), window, cx);
            return;
        };
        let store = self.store.clone();
        let saved: RunConfig = store
            .as_ref()
            .and_then(|s| s.get_setting(&run_key(workspace.id)).ok().flatten())
            .unwrap_or_default();
        let taken: Vec<u16> = self
            .runs
            .iter()
            .filter(|(id, _)| **id != workspace.id)
            .map(|(_, run)| run.port)
            .collect();
        let path = self.path.as_ref().map(|p| p.value.clone());
        let root = workspace.worktree_path.clone();
        let id = workspace.id;
        tracing::info!(workspace = %id, device = %name, "run");
        cx.spawn_in(window, async move |this, cx| {
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
                    let _ = this.update_in(cx, |this, window, cx| this.report(&err, window, cx));
                    return;
                }
            };
            if let Some(store) = &store {
                let config = RunConfig {
                    port: Some(prepared.port),
                    scheme: prepared.scheme.clone(),
                };
                if let Err(err) = store.set_setting(&run_key(id), &config) {
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
                this.start_run(id, root, prepared, name, handle, window, cx)
            });
        })
        .detach();
    }

    #[allow(clippy::too_many_arguments)]
    fn start_run(
        &mut self,
        id: WorkspaceId,
        root: PathBuf,
        prepared: Prepared,
        device: SharedString,
        handle: DeviceHandle,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self.projects.is_active(id) || self.busy.contains(&id) {
            return;
        }
        let Prepared {
            plan,
            stack,
            port,
            env,
            ..
        } = prepared;
        let previous = self.runs.remove(&id);
        let (mut server, old_build) = match previous {
            Some(run) => (run.server, run.build),
            None => (None, None),
        };
        if let Some(old) = old_build {
            old.kill();
        }
        // Keep a running dev server unless it serves another port.
        let keep_server = server
            .as_ref()
            .is_some_and(|s| s.running() && s.title.ends_with(&format!(":{port}")));
        if !keep_server {
            if let Some(old) = server.take() {
                old.kill();
            }
            if let Some(command) = &plan.server {
                server = self.spawn_tab(
                    id,
                    PaneTab::Server,
                    format!("Metro :{port}"),
                    command,
                    &root,
                    &env,
                    window,
                    cx,
                );
            }
        }
        let build = self.spawn_tab(
            id,
            PaneTab::Build,
            format!("Build · {stack}"),
            &plan.build,
            &root,
            &env,
            window,
            cx,
        );
        let started = build.is_some();
        self.runs.insert(
            id,
            WorkspaceRun {
                stack,
                port,
                server,
                build,
                root,
                after: plan.after,
                device,
                handle: Some(handle),
                phase: AfterPhase::Pending,
            },
        );
        if started {
            self.pane_tabs.insert(id, PaneTab::Build);
            self.focus_terminal(window, cx);
        }
        cx.notify();
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
        let Some(run) = self.runs.get_mut(&id) else {
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
        cx.notify();
        if kind == PaneTab::Build && status == (SessionStatus::Exited { code: 0 }) {
            self.after_build(id, window, cx);
        }
    }

    /// Installs and launches what a native build produced.
    fn after_build(&mut self, id: WorkspaceId, window: &mut Window, cx: &mut Context<Self>) {
        let Some(run) = self.runs.get_mut(&id) else {
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
                match result {
                    Ok(apps) => {
                        tracing::info!(?apps, %device, "run: launched");
                        window.push_notification(
                            Notification::success(format!(
                                "Launched {} on {device}",
                                apps.join(", ")
                            )),
                            cx,
                        );
                    }
                    Err(err) => {
                        tracing::warn!(%err, "run: install/launch failed");
                        this.report(&err, window, cx);
                    }
                }
                if let Some(run) = this.runs.get_mut(&id) {
                    run.phase = if ok {
                        AfterPhase::Launched
                    } else {
                        AfterPhase::Failed
                    };
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// ⌘.: stops the focused workspace's build and dev server.
    pub(super) fn stop_run(&mut self, cx: &mut Context<Self>) {
        if let Some(id) = self.panes.slot(self.panes.focused())
            && let Some(run) = self.runs.get(&id)
        {
            run.stop();
            cx.notify();
        }
    }

    /// Restarts the focused workspace's dev server on its port.
    pub(super) fn restart_server(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(id) = self.panes.slot(self.panes.focused()) else {
            return;
        };
        if let Some(server) = self.runs.get_mut(&id).and_then(|run| run.server.take()) {
            server.kill();
        }
        self.run_focused(window, cx);
    }

    /// Kills the workspace's run commands and frees its port (archive, delete).
    pub(super) fn close_run(&mut self, id: WorkspaceId) {
        if let Some(run) = self.runs.remove(&id) {
            run.stop();
        }
        self.pane_tabs.remove(&id);
    }

    /// Drops the saved port so another workspace can take it (delete).
    pub(super) fn forget_run_config(&self, id: WorkspaceId) {
        if let Some(store) = &self.store
            && let Err(err) = store.delete_setting(&run_key(id))
        {
            tracing::warn!(%err, "failed to forget the run config");
        }
    }

    /// The tab a pane shows for `id`, falling back to the agent when that run tab is gone.
    pub(super) fn pane_tab(&self, id: WorkspaceId) -> PaneTab {
        let tab = self.pane_tabs.get(&id).copied().unwrap_or_default();
        let run = self.runs.get(&id);
        let exists = match tab {
            PaneTab::Agent => true,
            PaneTab::Server => run.is_some_and(|r| r.server.is_some()),
            PaneTab::Build => run.is_some_and(|r| r.build.is_some()),
        };
        if exists { tab } else { PaneTab::Agent }
    }

    pub(super) fn run_view(&self, id: WorkspaceId, tab: PaneTab) -> Option<Entity<TerminalView>> {
        let run = self.runs.get(&id)?;
        match tab {
            PaneTab::Server => run.server.as_ref().map(|t| t.view.clone()),
            PaneTab::Build => run.build.as_ref().map(|t| t.view.clone()),
            PaneTab::Agent => None,
        }
    }
}
