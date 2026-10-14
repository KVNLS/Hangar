//! `HANGAR_DEV_*` automation hooks (documented in `main.rs`).

use super::*;

impl HangarApp {
    pub(super) fn dev_open(
        &mut self,
        repo: PathBuf,
        dev: DevOptions,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Task<Vec<WorkspaceId>> {
        tracing::info!(repo = %repo.display(), "HANGAR_DEV_OPEN");
        let add = self.add_project(ops::canonical(&repo), window, cx);
        cx.spawn_in(window, async move |this, cx| {
            let Some(project) = add.await else {
                return Vec::new();
            };
            let names = if dev.workspaces.is_empty() {
                vec![dev.workspace.clone()]
            } else {
                dev.workspaces.clone()
            };
            let mut created = Vec::new();
            for name in names {
                let (agent, target) = (dev.agent.clone(), dev.target.clone());
                let Ok(create) = this.update_in(cx, |this, window, cx| {
                    this.create_workspace(project.id, name, agent, target, window, cx)
                }) else {
                    break;
                };
                let Some(workspace_id) = create.await else {
                    continue;
                };
                created.push(workspace_id);
                if let Some(input) = dev.input.clone() {
                    let _ = this.update(cx, |this, _| {
                        let Some(live) = this.sessions.get(&workspace_id) else {
                            return;
                        };
                        if let Err(err) = live.handle.session().write(format!("{input}\r")) {
                            tracing::error!(%err, "HANGAR_DEV_INPUT: write failed");
                        }
                    });
                }
            }
            created
        })
    }
}

/// Debug/automation hooks, read once at startup (documented in `main.rs`).
#[derive(Clone, Debug, Default)]
pub struct DevOptions {
    pub open: Option<PathBuf>,
    pub workspace: String,
    /// Several workspaces to create instead of `workspace` (comma-separated names).
    pub workspaces: Vec<String>,
    /// Base device of the created workspaces (`ios:<UDID>` or `android:<AVD id>`).
    pub target: Option<crate::device_panel::DeviceKey>,
    /// "Devices at once" for this run (`auto` or 1–8), not saved.
    pub device_limit: Option<crate::device_pool::policy::Limit>,
    /// After the runs: save each dev workspace's latest frame here and log its device status.
    pub pool_frames: Option<PathBuf>,
    pub agent: String,
    pub input: Option<String>,
    pub archive: Option<String>,
    pub restore: Option<String>,
    pub delete: Option<String>,
    pub delete_branch: bool,
    pub delete_force: bool,
    pub remove_project: Option<PathBuf>,
    /// `1`, or a worktree-relative path to select once it changes.
    pub changes: Option<String>,
    /// Commit message: stage everything in the dev workspace, then commit.
    pub commit: Option<String>,
    pub push: bool,
    /// PR title (`1` = the default title).
    pub pr: Option<String>,
    /// Open the full-width review screen once the changes panel lists files.
    pub review: bool,
    /// Open the overview, log what it counts, then go back.
    pub overview: bool,
    /// Worktree-relative path to open in the Files tab.
    pub files: Option<String>,
    /// `dark`, `light` or `system`: render every settings page, then set the appearance.
    pub appearance: Option<String>,
    /// Screen to leave the window on once every other hook has run (for screenshots).
    pub show: Option<String>,
    /// Build and run the dev workspace once the device panel has a device open.
    pub run: bool,
    pub merge: Option<MergeStrategy>,
    /// Device id (simulator UDID) to open in the device panel.
    pub device: Option<String>,
    /// Press Home once the device shows frames (before the tap).
    pub device_home: bool,
    /// Tap at these device points once the device shows frames, then log whether the painted
    /// frame changed.
    pub device_tap: Option<(f64, f64)>,
    /// Type this text into the device once it shows frames (same check as the tap).
    pub device_text: Option<String>,
    /// Boot the device first when it is a shut-down virtual device (simulator UDID, AVD id).
    pub device_boot: bool,
    /// Comma-separated hardware controls to press after the text (`lock`, `volume-up`, ...).
    pub device_buttons: Vec<hangar_devices::Input>,
    /// Comma-separated keys to press after the buttons (`Backspace`, `Enter`, `ArrowUp`, ...).
    pub device_keys: Vec<hangar_devices::Input>,
    /// Shut the device down after all other device hooks.
    pub device_shutdown: bool,
    /// Open the logs pane under the device screen.
    pub device_logs: bool,
}

/// Parses a comma-separated list; unknown names are logged and skipped.
fn parse_list(
    hook: &str,
    value: Option<String>,
    parse: impl Fn(&str) -> Option<hangar_devices::Input>,
) -> Vec<hangar_devices::Input> {
    let value = value.unwrap_or_default();
    value
        .split(',')
        .map(str::trim)
        .filter(|name| !name.is_empty())
        .filter_map(|name| {
            let parsed = parse(name);
            if parsed.is_none() {
                tracing::error!(%name, "{hook}: unknown name");
            }
            parsed
        })
        .collect()
}

fn parse_button(name: &str) -> Option<hangar_devices::Input> {
    use hangar_devices::Input;
    Some(match name {
        "home" => Input::Home,
        "back" => Input::Back,
        "lock" | "power" => Input::Lock,
        "volume-up" => Input::VolumeUp,
        "volume-down" => Input::VolumeDown,
        "app-switch" => Input::AppSwitch,
        _ => return None,
    })
}

fn parse_key(name: &str) -> Option<hangar_devices::Input> {
    use hangar_devices::Key;
    Some(hangar_devices::Input::Key(
        match name.to_ascii_lowercase().as_str() {
            "backspace" => Key::Backspace,
            "enter" => Key::Enter,
            "tab" => Key::Tab,
            "escape" => Key::Escape,
            "arrowup" | "up" => Key::ArrowUp,
            "arrowdown" | "down" => Key::ArrowDown,
            "arrowleft" | "left" => Key::ArrowLeft,
            "arrowright" | "right" => Key::ArrowRight,
            _ => return None,
        },
    ))
}

impl DevOptions {
    pub fn from_env() -> Self {
        let var = |key: &str| std::env::var(key).ok().filter(|v| !v.is_empty());
        let flag = |key: &str| var(key).is_some_and(|v| v == "1");
        Self {
            open: var("HANGAR_DEV_OPEN").map(PathBuf::from),
            workspace: var("HANGAR_DEV_WORKSPACE").unwrap_or_else(|| "dev".to_owned()),
            workspaces: var("HANGAR_DEV_WORKSPACES")
                .map(|v| {
                    v.split(',')
                        .map(str::trim)
                        .filter(|n| !n.is_empty())
                        .map(str::to_owned)
                        .collect()
                })
                .unwrap_or_default(),
            target: var("HANGAR_DEV_TARGET").and_then(|v| {
                let parsed = v.split_once(':').and_then(|(p, id)| {
                    let platform = match p {
                        "ios" => crate::device_panel::Platform::Ios,
                        "android" => crate::device_panel::Platform::Android,
                        _ => return None,
                    };
                    Some(crate::device_panel::DeviceKey {
                        platform,
                        id: id.to_owned(),
                    })
                });
                if parsed.is_none() {
                    tracing::error!(value = %v, "HANGAR_DEV_TARGET: expected ios:<UDID> or android:<AVD id>");
                }
                parsed
            }),
            device_limit: var("HANGAR_DEV_DEVICE_LIMIT").and_then(|v| {
                let parsed = crate::device_pool::policy::Limit::parse(&v);
                if parsed.is_none() {
                    tracing::error!(value = %v, "HANGAR_DEV_DEVICE_LIMIT: expected auto or 1-8");
                }
                parsed
            }),
            pool_frames: var("HANGAR_DEV_POOL_FRAMES").map(PathBuf::from),
            agent: var("HANGAR_DEV_AGENT").unwrap_or_else(|| agents::FALLBACK_AGENT.to_owned()),
            input: var("HANGAR_DEV_INPUT"),
            archive: var("HANGAR_DEV_ARCHIVE"),
            restore: var("HANGAR_DEV_RESTORE"),
            delete: var("HANGAR_DEV_DELETE"),
            delete_branch: flag("HANGAR_DEV_DELETE_BRANCH"),
            delete_force: flag("HANGAR_DEV_DELETE_FORCE"),
            remove_project: var("HANGAR_DEV_REMOVE_PROJECT").map(PathBuf::from),
            changes: var("HANGAR_DEV_CHANGES"),
            commit: var("HANGAR_DEV_COMMIT"),
            push: flag("HANGAR_DEV_PUSH"),
            pr: var("HANGAR_DEV_PR"),
            review: flag("HANGAR_DEV_REVIEW"),
            overview: flag("HANGAR_DEV_OVERVIEW"),
            files: var("HANGAR_DEV_FILES"),
            appearance: var("HANGAR_DEV_APPEARANCE"),
            show: var("HANGAR_DEV_SHOW"),
            run: flag("HANGAR_DEV_RUN"),
            merge: var("HANGAR_DEV_MERGE").and_then(|v| match v.as_str() {
                "noff" => Some(MergeStrategy::NoFf),
                "squash" => Some(MergeStrategy::Squash),
                other => {
                    tracing::error!(value = other, "HANGAR_DEV_MERGE: expected noff or squash");
                    None
                }
            }),
            device: var("HANGAR_DEV_DEVICE"),
            device_home: flag("HANGAR_DEV_DEVICE_HOME"),
            device_text: var("HANGAR_DEV_DEVICE_TEXT"),
            device_boot: flag("HANGAR_DEV_DEVICE_BOOT"),
            device_buttons: parse_list(
                "HANGAR_DEV_DEVICE_BUTTONS",
                var("HANGAR_DEV_DEVICE_BUTTONS"),
                parse_button,
            ),
            device_keys: parse_list(
                "HANGAR_DEV_DEVICE_KEYS",
                var("HANGAR_DEV_DEVICE_KEYS"),
                parse_key,
            ),
            device_shutdown: flag("HANGAR_DEV_DEVICE_SHUTDOWN"),
            device_logs: flag("HANGAR_DEV_DEVICE_LOGS"),
            device_tap: var("HANGAR_DEV_DEVICE_TAP").and_then(|v| {
                let parsed = v
                    .split_once(',')
                    .and_then(|(x, y)| Some((x.trim().parse().ok()?, y.trim().parse().ok()?)));
                if parsed.is_none() {
                    tracing::error!(value = %v, "HANGAR_DEV_DEVICE_TAP: expected X,Y in points");
                }
                parsed
            }),
        }
    }
}

/// Runs the `HANGAR_DEV_*` hooks in order: open, archive, restore, delete, remove project.
pub(super) async fn run_dev_hooks(
    this: WeakEntity<HangarApp>,
    dev: DevOptions,
    cx: &mut AsyncWindowContext,
) {
    if let Some(changes) = dev.changes.clone() {
        tracing::info!(%changes, "HANGAR_DEV_CHANGES");
        let _ = this.update_in(cx, |this, window, cx| {
            if changes != "1" {
                this.changes_panel
                    .update(cx, |panel, _| panel.prefer(PathBuf::from(changes)));
            }
            this.set_changes_open(true, window, cx)
        });
    }
    if let Some(id) = dev.device.clone() {
        let mut inputs = Vec::new();
        if dev.device_home {
            inputs.push((
                "HANGAR_DEV_DEVICE_HOME",
                DevAction::Input(hangar_devices::Input::Home),
            ));
        }
        if let Some((x, y)) = dev.device_tap {
            inputs.push((
                "HANGAR_DEV_DEVICE_TAP",
                DevAction::Input(hangar_devices::Input::Tap { x, y }),
            ));
        }
        if let Some(text) = dev.device_text.clone() {
            inputs.push(("HANGAR_DEV_DEVICE_TEXT", DevAction::Text(text)));
        }
        for input in dev.device_buttons.iter().chain(&dev.device_keys) {
            inputs.push(("HANGAR_DEV_DEVICE_INPUT", DevAction::Input(*input)));
        }
        let boot = dev.device_boot;
        let shutdown = dev.device_shutdown;
        let logs = dev.device_logs;
        let this = this.clone();
        // Runs alongside the other hooks: waiting for frames must not hold them up.
        cx.spawn(async move |cx| dev_device(this, id, boot, logs, shutdown, inputs, cx).await)
            .detach();
    }
    let mut created = Vec::new();
    if let Some(repo) = dev.open.clone() {
        let open = dev.clone();
        if let Ok(task) =
            this.update_in(cx, |this, window, cx| this.dev_open(repo, open, window, cx))
        {
            created = task.await;
        }
    }
    if dev.run {
        dev_run(&this, &created, cx).await;
    }
    if let Some(dir) = dev.pool_frames.clone() {
        dev_pool_frames(&this, &created, &dir, cx).await;
    }
    run_review_hooks(&this, &dev, cx).await;
    if dev.review {
        dev_review(&this, cx).await;
    }
    if let Some(name) = dev.appearance.clone() {
        dev_settings(&this, &name, cx).await;
    }
    if let Some(path) = dev.files.clone() {
        let _ = this.update_in(cx, |this, window, cx| this.set_files_open(true, window, cx));
        cx.background_executor()
            .timer(Duration::from_millis(800))
            .await;
        let _ = this.update(cx, |this, cx| {
            this.files_panel
                .update(cx, |panel, cx| panel.open(PathBuf::from(path), cx))
        });
        cx.background_executor()
            .timer(Duration::from_millis(500))
            .await;
        let _ = this.update(cx, |this, cx| {
            let (files, rows, lines) = this.files_panel.read(cx).stats();
            tracing::info!(
                files,
                rows,
                ?lines,
                open = this.files_open,
                "HANGAR_DEV_FILES"
            );
        });
    }
    if dev.overview {
        let _ = this.update_in(cx, |this, window, cx| {
            this.set_screen(Screen::Overview, window, cx)
        });
        cx.background_executor()
            .timer(Duration::from_millis(500))
            .await;
        let _ = this.update_in(cx, |this, window, cx| {
            let counts = this.state_counts(cx);
            tracing::info!(?counts, screen = ?this.screen, "HANGAR_DEV_OVERVIEW");
            this.focus_next_needing_you(window, cx);
            let focused = this.focused_workspace().map(|(_, ws)| ws.slug.clone());
            tracing::info!(?focused, screen = ?this.screen, "HANGAR_DEV_OVERVIEW next needing you");
            this.set_screen(Screen::Workspace, window, cx);
        });
    }
    let find = |this: &mut HangarApp, hook: &str, slug: &str| {
        let found = this.workspace_by_slug(slug);
        if found.is_none() {
            tracing::error!(%slug, "{hook}: no such workspace");
        }
        found
    };
    if let Some(slug) = dev.archive.clone() {
        tracing::info!(%slug, "HANGAR_DEV_ARCHIVE");
        let task = this.update_in(cx, |this, window, cx| {
            find(this, "HANGAR_DEV_ARCHIVE", &slug).map(|id| this.archive_workspace(id, window, cx))
        });
        if let Ok(Some(task)) = task {
            task.await;
        }
    }
    if let Some(slug) = dev.restore.clone() {
        tracing::info!(%slug, "HANGAR_DEV_RESTORE");
        let task = this.update_in(cx, |this, window, cx| {
            find(this, "HANGAR_DEV_RESTORE", &slug).map(|id| this.restore_workspace(id, window, cx))
        });
        if let Ok(Some(task)) = task {
            task.await;
        }
    }
    if let Some(slug) = dev.delete.clone() {
        let options = DeleteOptions {
            force: dev.delete_force,
            delete_branch: dev.delete_branch,
        };
        tracing::info!(%slug, ?options, "HANGAR_DEV_DELETE");
        let task = this.update_in(cx, |this, window, cx| {
            find(this, "HANGAR_DEV_DELETE", &slug)
                .map(|id| this.delete_workspace(id, options, window, cx))
        });
        if let Ok(Some(task)) = task {
            task.await;
        }
    }
    if let Some(repo) = dev.remove_project.clone() {
        tracing::info!(repo = %repo.display(), "HANGAR_DEV_REMOVE_PROJECT");
        let repo = ops::canonical(&repo);
        let task = this.update_in(cx, |this, window, cx| {
            let id = this
                .projects
                .entries()
                .iter()
                .find(|e| ops::canonical(&e.project.repo_path) == repo)
                .map(|e| e.project.id);
            match id {
                Some(id) => Some(this.remove_project(id, window, cx)),
                None => {
                    tracing::error!(repo = %repo.display(), "HANGAR_DEV_REMOVE_PROJECT: no such project");
                    None
                }
            }
        });
        if let Ok(Some(task)) = task {
            task.await;
        }
    }
    if let Some(name) = dev.show.clone() {
        dev_show(&this, &name, cx).await;
    }
}

/// The workspaces created by `HANGAR_DEV_OPEN`, else the focused one.
fn dev_targets(
    this: &WeakEntity<HangarApp>,
    created: &[WorkspaceId],
    cx: &mut AsyncWindowContext,
) -> Vec<(WorkspaceId, String)> {
    this.update(cx, |this, _| {
        let ids: Vec<WorkspaceId> = if created.is_empty() {
            this.focused_workspace()
                .map(|(_, w)| w.id)
                .into_iter()
                .collect()
        } else {
            created.to_vec()
        };
        ids.into_iter()
            .filter_map(|id| this.projects.workspace(id).map(|w| (id, w.slug.clone())))
            .collect()
    })
    .unwrap_or_default()
}

/// `HANGAR_DEV_RUN`: builds and runs every dev workspace at once (as ⌘R on each), on each
/// workspace's own device, and logs each outcome (`HANGAR_DEV_RUN <slug> ...`) once all ended.
async fn dev_run(
    this: &WeakEntity<HangarApp>,
    created: &[WorkspaceId],
    cx: &mut AsyncWindowContext,
) {
    let targets = dev_targets(this, created, cx);
    if targets.is_empty() {
        tracing::error!("HANGAR_DEV_RUN: no workspace to run");
        return;
    }
    let mut runs = Vec::new();
    for (id, slug) in targets {
        tracing::info!(%slug, "HANGAR_DEV_RUN");
        if let Ok(task) = this.update_in(cx, |this, window, cx| this.run_workspace(id, window, cx))
        {
            runs.push((slug, std::time::Instant::now(), task));
        }
    }
    for (slug, started, task) in runs {
        match task.await {
            Ok(summary) => tracing::info!(
                %slug,
                %summary,
                secs = started.elapsed().as_secs(),
                "HANGAR_DEV_RUN done"
            ),
            Err(err) => {
                tracing::error!(%slug, secs = started.elapsed().as_secs(), "HANGAR_DEV_RUN failed: {err:#}")
            }
        }
    }
}

/// `HANGAR_DEV_POOL_FRAMES=<dir>`: through the device API, logs each dev workspace's device
/// status, saves its latest frame as `<dir>/<slug>.png`, taps the screen centre and saves the
/// next frame as `<slug>-tap.png`.
async fn dev_pool_frames(
    this: &WeakEntity<HangarApp>,
    created: &[WorkspaceId],
    dir: &std::path::Path,
    cx: &mut AsyncWindowContext,
) {
    if let Err(err) = std::fs::create_dir_all(dir) {
        tracing::error!(%err, "HANGAR_DEV_POOL_FRAMES: cannot create {}", dir.display());
        return;
    }
    for (id, slug) in dev_targets(this, created, cx) {
        let Ok(status) = this.update(cx, |this, cx| this.device_status(id, cx)) else {
            return;
        };
        match status.await {
            Ok(status) => tracing::info!(%slug, ?status, "HANGAR_DEV_POOL_FRAMES status"),
            Err(err) => tracing::error!(%slug, "HANGAR_DEV_POOL_FRAMES status: {err:#}"),
        }
        let Ok(frame) = this.update(cx, |this, cx| this.latest_frame(id, cx)) else {
            return;
        };
        let shot = match frame.await {
            Ok(shot) => shot,
            Err(err) => {
                tracing::error!(%slug, "HANGAR_DEV_POOL_FRAMES frame: {err:#}");
                continue;
            }
        };
        let path = dir.join(format!("{slug}.png"));
        match shot.frame.save_png(&path) {
            Ok(()) => tracing::info!(
                %slug,
                path = %path.display(),
                width = shot.frame.width,
                height = shot.frame.height,
                width_pt = shot.width_pt,
                height_pt = shot.height_pt,
                "HANGAR_DEV_POOL_FRAMES saved"
            ),
            Err(err) => tracing::error!(%slug, "HANGAR_DEV_POOL_FRAMES save: {err}"),
        }
        let tap = hangar_devices::Input::Tap {
            x: shot.width_pt / 2.0,
            y: shot.height_pt / 2.0,
        };
        let Ok(sent) = this.update(cx, |this, cx| this.send_input(id, tap, cx)) else {
            return;
        };
        match sent.await {
            Ok(()) => tracing::info!(%slug, ?tap, "HANGAR_DEV_POOL_FRAMES tap sent"),
            Err(err) => tracing::error!(%slug, "HANGAR_DEV_POOL_FRAMES tap: {err:#}"),
        }
        cx.background_executor().timer(Duration::from_secs(1)).await;
        if let Ok(frame) = this.update(cx, |this, cx| this.latest_frame(id, cx))
            && let Ok(after) = frame.await
        {
            let path = dir.join(format!("{slug}-tap.png"));
            let diff = after.frame.diff_ratio(&shot.frame);
            let _ = after.frame.save_png(&path);
            tracing::info!(%slug, diff_percent = diff * 100.0, path = %path.display(), "HANGAR_DEV_POOL_FRAMES after tap");
        }
        let Ok(logs) = this.update(cx, |this, cx| this.recent_logs(id, 5, None, None, cx)) else {
            return;
        };
        match logs.await {
            Ok(text) => {
                tracing::info!(%slug, lines = text.lines().count(), "HANGAR_DEV_POOL_FRAMES logs")
            }
            Err(err) => tracing::error!(%slug, "HANGAR_DEV_POOL_FRAMES logs: {err:#}"),
        }
    }
}

/// `HANGAR_DEV_SHOW`: leaves the window on one screen or overlay.
async fn dev_show(this: &WeakEntity<HangarApp>, name: &str, cx: &mut AsyncWindowContext) {
    use super::settings::SettingsTab;
    if name.starts_with("review") {
        // The review screen only lists files once the changes panel has loaded them.
        let deadline = std::time::Instant::now() + Duration::from_secs(15);
        while this
            .update(cx, |this, cx| {
                this.changes_panel.read(cx).review_stats().files == 0
            })
            .unwrap_or(false)
            && std::time::Instant::now() < deadline
        {
            cx.background_executor().timer(DEV_POLL).await;
        }
    }
    let shown = this.update_in(cx, |this, window, cx| {
        let settings = |tab| (Screen::Settings, Some(tab));
        let (screen, tab) = match name {
            "workspace" | "device" => (Screen::Workspace, None),
            "review" | "review-inline" => (Screen::Review, None),
            "overview" => (Screen::Overview, None),
            "settings" | "settings-agents" => settings(SettingsTab::Agents),
            "settings-devices" => settings(SettingsTab::Devices),
            "settings-appearance" => settings(SettingsTab::Appearance),
            "settings-keybindings" => settings(SettingsTab::Keybindings),
            "palette" | "sheet" => (Screen::Workspace, None),
            _ => return false,
        };
        this.set_screen(screen, window, cx);
        if let Some(tab) = tab {
            this.open_settings_tab(tab, window, cx);
        }
        if name.starts_with("review") {
            let split = name == "review";
            this.changes_panel
                .update(cx, |panel, cx| panel.set_split_view(split, cx));
        }
        match name {
            "device" => this.set_device_open(true, window, cx),
            "palette" => this.open_palette(window, cx),
            "sheet" => match this.current_project() {
                Some(project) => this.open_new_workspace(project, window, cx),
                None => tracing::error!("HANGAR_DEV_SHOW sheet: no project"),
            },
            _ => {}
        }
        true
    });
    match shown {
        Ok(true) => tracing::info!(%name, "HANGAR_DEV_SHOW"),
        Ok(false) => tracing::error!(
            %name,
            "HANGAR_DEV_SHOW: expected workspace, review, review-inline, overview, settings-<tab>, palette or sheet"
        ),
        Err(_) => {}
    }
}

/// How long the device hooks wait for the device list and for frames.
const DEV_DEVICE_TIMEOUT: Duration = Duration::from_secs(30);
const DEV_BOOT_TIMEOUT: Duration = Duration::from_secs(300);
const DEV_POLL: Duration = Duration::from_millis(100);

enum DevAction {
    Input(hangar_devices::Input),
    Text(String),
}

/// `HANGAR_DEV_DEVICE` (+ `_BOOT`, `_LOGS`, `_HOME`, `_TAP`, `_TEXT`): opens the device panel on the device, waits for a
/// painted frame, then sends each input and logs whether a later painted frame differs.
async fn dev_device(
    this: WeakEntity<HangarApp>,
    id: String,
    boot: bool,
    logs: bool,
    shutdown: bool,
    inputs: Vec<(&'static str, DevAction)>,
    cx: &mut AsyncWindowContext,
) {
    tracing::info!(%id, "HANGAR_DEV_DEVICE");
    let Ok(panel) = this.update_in(cx, |this, window, cx| {
        this.set_device_open(true, window, cx);
        this.device_panel.clone()
    }) else {
        return;
    };
    if logs {
        panel.update(cx, |panel, cx| panel.set_logs_open(true, cx));
    }
    let deadline = std::time::Instant::now() + DEV_DEVICE_TIMEOUT;
    let wait = |cx: &AsyncWindowContext| cx.background_executor().timer(DEV_POLL);
    // Wait for a listing that knows the device.
    let key = loop {
        let found = panel.update(cx, |panel, _| {
            let picker = panel.picker();
            (picker.listed() && !picker.is_listing()).then(|| picker.find_id(&id))
        });
        match found {
            Some(Some(key)) => break key,
            Some(None) => {
                tracing::error!(%id, "HANGAR_DEV_DEVICE: device not listed (is it booted?)");
                return;
            }
            None if std::time::Instant::now() < deadline => wait(cx).await,
            None => {
                tracing::error!(%id, "HANGAR_DEV_DEVICE: device list not ready");
                return;
            }
        }
    };
    let mut deadline = deadline;
    if boot {
        // Booting takes minutes; the panel selects and opens the device once it is up.
        deadline = std::time::Instant::now() + DEV_BOOT_TIMEOUT;
        let _ = panel.update_in(cx, |panel, window, cx| {
            panel.select_device(key.clone(), window, cx);
            panel.boot_device(key, window, cx);
        });
    } else {
        let _ = panel.update_in(cx, |panel, window, cx| panel.select_device(key, window, cx));
    }
    let painted = |cx: &mut AsyncWindowContext| {
        panel.update(cx, |panel, cx| {
            panel
                .view()
                .and_then(|view| view.read(cx).painted_frame().cloned())
        })
    };
    let baseline = loop {
        if let Some(frame) = painted(cx) {
            break frame;
        }
        if std::time::Instant::now() >= deadline {
            tracing::error!(%id, "HANGAR_DEV_DEVICE: no frame painted");
            return;
        }
        wait(cx).await;
    };
    tracing::info!(
        seq = baseline.seq,
        width = baseline.width,
        height = baseline.height,
        "HANGAR_DEV_DEVICE frame painted"
    );
    'inputs: for (hook, action) in inputs {
        // Let animations settle so the baseline is what the input acts on.
        cx.background_executor().timer(Duration::from_secs(2)).await;
        let Some(baseline) = painted(cx) else {
            return;
        };
        let sent_at = std::time::Instant::now();
        let sent = match &action {
            DevAction::Input(input) => panel.update(cx, |panel, cx| panel.send(*input, cx)),
            DevAction::Text(text) => {
                panel.update(cx, |panel, cx| panel.send_text(text.clone(), cx))
            }
        };
        if !sent {
            tracing::error!("{hook}: device closed");
            return;
        }
        match &action {
            DevAction::Input(input) => tracing::info!(?input, seq = baseline.seq, "{hook} sent"),
            DevAction::Text(text) => {
                tracing::info!(%text, seq = baseline.seq, "{hook} sent");
            }
        }
        let deadline = sent_at + Duration::from_secs(5);
        while std::time::Instant::now() < deadline {
            wait(cx).await;
            if let Some(err) = panel.read_with(cx, |panel, _| panel.input_error()) {
                tracing::error!(%err, "{hook}: input failed");
                continue 'inputs;
            }
            let Some(frame) = painted(cx) else {
                continue;
            };
            let diff = frame.diff_ratio(&baseline);
            if frame.seq != baseline.seq && diff > 0.002 {
                tracing::info!(
                    seq = frame.seq,
                    diff_percent = diff * 100.0,
                    after_ms = sent_at.elapsed().as_millis() as u64,
                    "{hook} painted frame changed"
                );
                continue 'inputs;
            }
        }
        tracing::error!("{hook}: painted frame did not change within 5 s");
    }
    if shutdown {
        tracing::info!(%id, "HANGAR_DEV_DEVICE_SHUTDOWN");
        panel.update(cx, |panel, cx| panel.shutdown_device(cx));
        let deadline = std::time::Instant::now() + DEV_BOOT_TIMEOUT;
        while std::time::Instant::now() < deadline {
            wait(cx).await;
            if panel.read_with(cx, |panel, _| panel.view().is_none()) {
                tracing::info!(%id, "HANGAR_DEV_DEVICE_SHUTDOWN done");
                return;
            }
        }
        tracing::error!(%id, "HANGAR_DEV_DEVICE_SHUTDOWN: device still open");
    }
}

/// `HANGAR_DEV_COMMIT`, `_PUSH`, `_PR`, `_MERGE`, in that order, on the workspace shown in the
/// changes panel (the `HANGAR_DEV_OPEN` one). Each goes through the panel's own operations and
/// logs its outcome.
async fn run_review_hooks(
    this: &WeakEntity<HangarApp>,
    dev: &DevOptions,
    cx: &mut AsyncWindowContext,
) {
    if dev.commit.is_none() && !dev.push && dev.pr.is_none() && dev.merge.is_none() {
        return;
    }
    let Ok(panel) = this.update(cx, |this, _| this.changes_panel.clone()) else {
        return;
    };
    let worktree = this
        .update(cx, |this, cx| {
            let id = panel.read(cx).workspace()?;
            this.projects
                .workspace(id)
                .map(|ws| ws.worktree_path.clone())
        })
        .ok()
        .flatten();
    let Some(worktree) = worktree else {
        tracing::error!("review hooks: no workspace in the changes panel");
        return;
    };
    let executor = cx.background_executor().clone();

    if let Some(message) = dev.commit.clone() {
        // The agent writes files asynchronously: stage until something is staged.
        let mut staged = 0;
        for _ in 0..50 {
            let task = panel.update(cx, |panel, cx| panel.stage_all(cx));
            if let Err(err) = task.await {
                tracing::error!("HANGAR_DEV_COMMIT stage all: {err:#}");
            }
            let path = worktree.clone();
            staged = executor
                .spawn(async move { hangar_worktree::status(&path).map(|s| s.staged) })
                .await
                .unwrap_or(0);
            if staged > 0 {
                break;
            }
            executor.timer(Duration::from_millis(300)).await;
        }
        tracing::info!(staged, "HANGAR_DEV_COMMIT staged");
        let task = panel.update(cx, |panel, cx| panel.commit(message, cx));
        match task.await {
            Ok(oid) => tracing::info!(%oid, "HANGAR_DEV_COMMIT committed"),
            Err(err) => tracing::error!("HANGAR_DEV_COMMIT: {err:#}"),
        }
    }
    if dev.push {
        let task = panel.update(cx, |panel, cx| panel.push(cx));
        match task.await {
            Ok(out) => tracing::info!(?out, "HANGAR_DEV_PUSH pushed"),
            Err(err) => tracing::error!("HANGAR_DEV_PUSH: {err:#}"),
        }
    }
    if let Some(title) = dev.pr.clone() {
        // The default title comes from the review info, loaded once gh is known.
        let mut defaults = None;
        for _ in 0..50 {
            defaults = panel.update(cx, |panel, _| {
                panel.pr_defaults().filter(|_| panel.info_loaded())
            });
            if defaults.is_some() {
                break;
            }
            executor.timer(Duration::from_millis(200)).await;
        }
        match defaults {
            None => tracing::error!("HANGAR_DEV_PR: review info never loaded"),
            Some((workspace, default_title)) => {
                let request = crate::changes::review::PrRequest {
                    title: if title == "1" { default_title } else { title },
                    body: "Opened by HANGAR_DEV_PR.".to_owned(),
                    draft: true,
                };
                let task = panel.update(cx, |panel, cx| panel.create_pr(workspace, request, cx));
                match task.await {
                    Ok(url) => tracing::info!(%url, "HANGAR_DEV_PR opened"),
                    Err(err) => tracing::error!("HANGAR_DEV_PR: {err:#}"),
                }
            }
        }
    }
    if let Some(strategy) = dev.merge {
        let task = panel.update(cx, |panel, cx| {
            let (workspace, message) = panel.merge_defaults(strategy)?;
            Some(panel.merge(workspace, strategy, message, cx))
        });
        match task {
            Some(task) => match task.await {
                Ok(out) => tracing::info!(?strategy, ?out, "HANGAR_DEV_MERGE merged"),
                Err(err) => tracing::error!(?strategy, "HANGAR_DEV_MERGE: {err:#}"),
            },
            None => tracing::error!("HANGAR_DEV_MERGE: no workspace in the changes panel"),
        }
    }
}

/// `HANGAR_DEV_REVIEW`: waits (up to 15 s) for the changes panel to list files, opens the review
/// screen, then logs what it shows in split and inline mode.
async fn dev_review(this: &WeakEntity<HangarApp>, cx: &mut AsyncWindowContext) {
    let stats = |this: &WeakEntity<HangarApp>, cx: &mut AsyncWindowContext| {
        this.update(cx, |this, cx| this.changes_panel.read(cx).review_stats())
            .ok()
    };
    let deadline = std::time::Instant::now() + Duration::from_secs(15);
    while stats(this, cx).is_some_and(|s| s.files == 0) && std::time::Instant::now() < deadline {
        cx.background_executor().timer(DEV_POLL).await;
    }
    let _ = this.update_in(cx, |this, window, cx| {
        this.set_screen(Screen::Review, window, cx)
    });
    cx.background_executor()
        .timer(Duration::from_millis(500))
        .await;
    match stats(this, cx) {
        Some(s) => tracing::info!(
            files = s.files,
            unified_rows = s.unified_rows,
            split_rows = s.split_rows,
            wide = s.wide,
            "HANGAR_DEV_REVIEW"
        ),
        None => tracing::error!("HANGAR_DEV_REVIEW: app gone"),
    }
    let _ = this.update(cx, |this, cx| {
        this.changes_panel
            .update(cx, |panel, cx| panel.set_split_view(false, cx))
    });
    cx.background_executor()
        .timer(Duration::from_millis(300))
        .await;
    let _ = this.update_in(cx, |this, window, cx| {
        this.set_screen(Screen::Workspace, window, cx)
    });
    tracing::info!("HANGAR_DEV_REVIEW inline rendered, back to workspace");
}

/// `HANGAR_DEV_APPEARANCE`: renders each settings page, then applies and saves the appearance.
async fn dev_settings(this: &WeakEntity<HangarApp>, name: &str, cx: &mut AsyncWindowContext) {
    use crate::ui::theme::Appearance;
    let appearance = match name {
        "dark" => Appearance::Dark,
        "light" => Appearance::Light,
        "system" => Appearance::System,
        other => {
            tracing::error!(%other, "HANGAR_DEV_APPEARANCE: expected dark, light or system");
            return;
        }
    };
    let _ = this.update_in(cx, |this, window, cx| {
        this.set_screen(Screen::Settings, window, cx)
    });
    for tab in super::settings::SettingsTab::ALL {
        let _ = this.update_in(cx, |this, window, cx| {
            this.open_settings_tab(tab, window, cx)
        });
        cx.background_executor()
            .timer(Duration::from_millis(200))
            .await;
    }
    let _ = this.update_in(cx, |this, window, cx| {
        this.set_appearance(appearance, window, cx);
        tracing::info!(appearance = ?this.appearance, dark = cx.theme().is_dark(), "HANGAR_DEV_APPEARANCE");
        this.set_screen(Screen::Workspace, window, cx);
    });
}
