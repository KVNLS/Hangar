//! The main window: project sidebar and two tiled terminal panes.

mod dialogs;
mod sidebar;

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use anyhow::anyhow;
use gpui_kit::component::button::{Button, ButtonVariant, ButtonVariants as _};
use gpui_kit::component::input::{InputEvent, InputState};
use gpui_kit::component::notification::Notification;
use gpui_kit::component::resizable::{ResizablePanelEvent, ResizableState};
use gpui_kit::component::select::{Select, SelectEvent, SelectItem, SelectState};
use gpui_kit::component::sidebar::{Sidebar, SidebarCollapsible};
use gpui_kit::component::{
    ActiveTheme as _, IconName, Selectable as _, Sizable as _, StyledExt as _, WindowExt as _,
    h_flex, h_resizable, resizable_panel, v_flex,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use hangar_agents::AgentHandle;
use hangar_core::{
    Project, ProjectId, Session, SessionStatus, Store, Workspace, WorkspaceId, WorkspaceStatus,
};
use hangar_terminal::ExitInfo;
use hangar_worktree::MergeStrategy;

use crate::agents::{self, AgentChoice};
use crate::changes::review::ReviewTarget;
use crate::changes::{ChangesEvent, ChangesPanel, PanelSource, WorkspaceDiff};
use crate::device_panel::{DevicePanel, DevicePanelEvent};
use crate::login_env::{self, PathSource, ResolvedPath};
use crate::ops::{self, DeleteOptions, Loaded, StartedSession};
use crate::paths;
use crate::state::{
    CHANGES_WIDTH_RANGE, ChangesLayout, DEVICE_WIDTH_RANGE, DeviceLayout, PANE_COUNT, PaneLayout,
    Panes, ProjectList, changes_width, device_width, sanitize_split, split_ratio, status_label,
};
use crate::terminal_view::{TerminalEvent, TerminalView, TerminalViewOptions, TerminalWaker};
use dialogs::DeleteWorkspaceDialog;
use sidebar::{ProjectSection, WorkspaceRow};

/// Width of the sidebar; the panes share the rest.
const SIDEBAR_WIDTH: f32 = 260.;
/// How long a killed agent gets to exit before its worktree is removed.
const KILL_GRACE: Duration = Duration::from_secs(3);
/// The panes keep at least this much width when the changes panel is dragged wider.
const MIN_PANES_WIDTH: f32 = 320.;
/// Quiet time after a panel resize before the width is saved.
const SAVE_DEBOUNCE: Duration = Duration::from_millis(400);

actions!(hangar, [ToggleChanges, ToggleDevice, ToggleDeviceStats]);

pub fn init(cx: &mut App) {
    cx.bind_keys([
        KeyBinding::new("cmd-shift-d", ToggleChanges, None),
        KeyBinding::new("cmd-shift-p", ToggleDevice, None),
        KeyBinding::new("cmd-alt-shift-p", ToggleDeviceStats, None),
    ]);
}

/// Debug/automation hooks, read once at startup (documented in `main.rs`).
#[derive(Clone, Debug, Default)]
pub struct DevOptions {
    pub open: Option<PathBuf>,
    pub workspace: String,
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

#[derive(Clone, Debug, PartialEq)]
struct AgentOption {
    id: SharedString,
    title: SharedString,
}

impl SelectItem for AgentOption {
    type Value = SharedString;

    fn title(&self) -> SharedString {
        self.title.clone()
    }

    fn value(&self) -> &Self::Value {
        &self.id
    }
}

type AgentSelect = SelectState<Vec<AgentOption>>;

struct LiveSession {
    session: Session,
    agent_name: SharedString,
    /// Owns the process; dropping it kills the program.
    handle: AgentHandle,
    view: Entity<TerminalView>,
    _exit: Subscription,
}

impl LiveSession {
    /// Kills the program if it still runs. Returns the session to record as stopped, if any.
    fn stop(&self) -> Option<hangar_core::SessionId> {
        if self.session.status.is_terminal() {
            return None;
        }
        if let Err(err) = self.handle.session().kill() {
            tracing::warn!(%err, session = %self.session.id, "failed to kill agent");
        }
        Some(self.session.id)
    }
}

struct DiffEntry {
    diff: Entity<WorkspaceDiff>,
    _observe: Subscription,
}

/// Drag payload of the changes panel's resize handle.
struct ChangesResize;

impl Render for ChangesResize {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        Empty
    }
}

/// Drag payload of the device panel's resize handle.
struct DeviceResize;

impl Render for DeviceResize {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        Empty
    }
}

struct NewWorkspaceInput {
    project_id: ProjectId,
    input: Entity<InputState>,
    _submit: Subscription,
}

pub struct HangarApp {
    store: Option<Arc<Store>>,
    projects: ProjectList,
    loading: bool,
    load_error: Option<SharedString>,
    path: Option<ResolvedPath>,
    agent_select: Entity<AgentSelect>,
    agent: SharedString,
    agent_chosen_by_user: bool,
    sessions: HashMap<WorkspaceId, LiveSession>,
    panes: Panes,
    pane_sizes: Entity<ResizableState>,
    /// First pane's share of the width, once the user has resized (or it was restored).
    split: Option<f32>,
    new_workspace: Option<NewWorkspaceInput>,
    creating: HashSet<ProjectId>,
    /// Workspaces with an archive, restore or delete in flight.
    busy: HashSet<WorkspaceId>,
    /// Live diffs of active workspaces, started when first shown in a pane.
    diffs: HashMap<WorkspaceId, DiffEntry>,
    changes: ChangesLayout,
    changes_panel: Entity<ChangesPanel>,
    save_changes: Option<Task<()>>,
    /// The window's single device panel, shared by all workspaces.
    device: DeviceLayout,
    device_panel: Entity<DevicePanel>,
    save_device: Option<Task<()>>,
    focus: FocusHandle,
    _subscriptions: Vec<Subscription>,
}

impl HangarApp {
    pub fn new(dev: DevOptions, window: &mut Window, cx: &mut Context<Self>) -> Self {
        // Install state is unknown until the login PATH arrives; list names only for now.
        let options = agent_options(
            &hangar_agents::presets()
                .into_iter()
                .map(|a| AgentChoice {
                    id: a.id,
                    name: a.display_name,
                    installed: true,
                })
                .collect::<Vec<_>>(),
        );
        let agent: SharedString = agents::FALLBACK_AGENT.into();
        let selected = options.iter().position(|o| o.id == agent);
        let agent_select = cx.new(|cx| {
            SelectState::new(
                options,
                selected.map(|row| gpui_kit::component::IndexPath::default().row(row)),
                window,
                cx,
            )
        });
        let select_sub = cx.subscribe_in(
            &agent_select,
            window,
            |this, _, event: &SelectEvent<Vec<AgentOption>>, window, cx| {
                let SelectEvent::Confirm(Some(id)) = event else {
                    return;
                };
                this.agent = id.clone();
                this.agent_chosen_by_user = true;
                this.save_agent(window, cx);
                cx.notify();
            },
        );
        let pane_sizes = cx.new(|_| ResizableState::default());
        let resize_sub = cx.subscribe_in(
            &pane_sizes,
            window,
            |this, sizes, _: &ResizablePanelEvent, window, cx| {
                let sizes = sizes.read(cx);
                let first = sizes.sizes().first().map(|s| s.as_f32());
                let total = sizes.container_size().as_f32();
                if let Some(split) = first.and_then(|first| split_ratio(first, total)) {
                    this.split = Some(split);
                    this.save_layout(window, cx);
                }
            },
        );

        let changes_panel = cx.new(ChangesPanel::new);
        let changes_sub = cx.subscribe_in(
            &changes_panel,
            window,
            |this, _, event: &ChangesEvent, window, cx| match event {
                ChangesEvent::Close => this.set_changes_open(false, window, cx),
            },
        );
        let device_panel = cx.new(|cx| DevicePanel::new(window, cx));
        let device_sub = cx.subscribe_in(
            &device_panel,
            window,
            |this, _, event: &DevicePanelEvent, window, cx| match event {
                DevicePanelEvent::Close => this.set_device_open(false, window, cx),
                DevicePanelEvent::Selected(key) => {
                    this.device.selected = key.clone();
                    this.save_device_layout(window, cx);
                }
                DevicePanelEvent::Stats(stats) => {
                    this.device.stats = *stats;
                    this.save_device_layout(window, cx);
                }
                DevicePanelEvent::Logs(logs) => {
                    this.device.logs = logs.open;
                    this.device.log_level = Some(logs.min_level.as_str().to_owned());
                    this.save_device_layout(window, cx);
                }
            },
        );
        let focus = cx.focus_handle();
        window.focus(&focus, cx);

        let db = paths::db_path();
        cx.spawn_in(window, async move |this, cx| {
            let executor = cx.background_executor().clone();
            let path_task = executor.spawn(async { login_env::resolve() });
            let loaded = executor
                .spawn(async move {
                    let db = db.ok_or_else(|| {
                        anyhow!("cannot locate the database: HOME is not set (set HANGAR_DB)")
                    })?;
                    let store = ops::open_store(&db)?;
                    let loaded = ops::load(&store)?;
                    tracing::info!(
                        db = %db.display(),
                        projects = loaded.entries.len(),
                        layout = ?loaded.layout,
                        agent = ?loaded.agent,
                        "loaded store"
                    );
                    anyhow::Ok((store, loaded))
                })
                .await;
            let _ = this.update_in(cx, |this, window, cx| this.on_loaded(loaded, window, cx));
            let path = path_task.await;
            let _ = this.update_in(cx, |this, window, cx| this.on_path(path, window, cx));
            run_dev_hooks(this, dev, cx).await;
        })
        .detach();

        Self {
            store: None,
            projects: ProjectList::default(),
            loading: true,
            load_error: None,
            path: None,
            agent_select,
            agent,
            agent_chosen_by_user: false,
            sessions: HashMap::new(),
            panes: Panes::default(),
            pane_sizes,
            split: None,
            new_workspace: None,
            creating: HashSet::new(),
            busy: HashSet::new(),
            diffs: HashMap::new(),
            changes: ChangesLayout::default(),
            changes_panel,
            save_changes: None,
            device: DeviceLayout::default(),
            device_panel,
            save_device: None,
            focus,
            _subscriptions: vec![select_sub, resize_sub, changes_sub, device_sub],
        }
    }

    fn report(&mut self, err: &anyhow::Error, window: &mut Window, cx: &mut Context<Self>) {
        tracing::error!("{err:#}");
        window.push_notification(Notification::error(format!("{err:#}")), cx);
    }

    /// Runs `job` with the store on the background executor and reports its error, if any.
    fn persist(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
        job: impl FnOnce(&Store) -> anyhow::Result<()> + Send + 'static,
    ) {
        let Some(store) = self.store.clone() else {
            return;
        };
        cx.spawn_in(window, async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn(async move { job(&store) })
                .await;
            if let Err(err) = result {
                let _ = this.update_in(cx, |this, window, cx| this.report(&err, window, cx));
            }
        })
        .detach();
    }

    fn save_layout(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let layout = self.panes.layout(self.split);
        self.persist(window, cx, move |store| ops::save_layout(store, &layout));
    }

    /// Saves the pane layout and starts/stops live diffs to match what the panes show.
    fn panes_changed(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.save_layout(window, cx);
        self.sync_diffs(cx);
    }

    /// Starts a live diff for every active workspace on screen, stops those of workspaces
    /// that are gone, archived or being removed, and points the changes panel at the focused
    /// pane's workspace.
    fn sync_diffs(&mut self, cx: &mut Context<Self>) {
        let projects = &self.projects;
        let busy = &self.busy;
        self.diffs
            .retain(|id, _| projects.is_active(*id) && !busy.contains(id));
        for pane in 0..PANE_COUNT {
            let Some(id) = self.panes.slot(pane) else {
                continue;
            };
            if self.diffs.contains_key(&id) || self.busy.contains(&id) {
                continue;
            }
            let Some(ws) = self
                .projects
                .workspace(id)
                .filter(|_| self.projects.is_active(id))
            else {
                continue;
            };
            let (path, base) = (ws.worktree_path.clone(), ws.base.clone());
            let diff = cx.new(|cx| WorkspaceDiff::start(id, path, base, cx));
            let observe = cx.observe(&diff, |_, _, cx| cx.notify());
            self.diffs.insert(
                id,
                DiffEntry {
                    diff,
                    _observe: observe,
                },
            );
        }
        let source = self.panes.slot(self.panes.focused()).and_then(|id| {
            let entry = self.diffs.get(&id)?;
            let ws = self.projects.workspace(id)?;
            let project = self.projects.project(ws.project_id)?;
            Some(PanelSource {
                workspace: id,
                name: SharedString::from(ws.name.clone()),
                diff: entry.diff.clone(),
                target: ReviewTarget {
                    workspace: ws.clone(),
                    repo: project.repo_path.clone(),
                },
            })
        });
        self.changes_panel
            .update(cx, |panel, cx| panel.set_source(source, cx));
    }

    fn set_changes_open(&mut self, open: bool, window: &mut Window, cx: &mut Context<Self>) {
        if self.changes.open != open {
            self.changes.open = open;
            tracing::info!(open, "changes panel");
            self.save_changes_layout(window, cx);
            cx.notify();
        }
    }

    fn toggle_changes(&mut self, _: &ToggleChanges, window: &mut Window, cx: &mut Context<Self>) {
        self.set_changes_open(!self.changes.open, window, cx);
    }

    /// The pane header button: opens the panel on this pane, or closes it if it already
    /// shows this pane.
    fn toggle_changes_for(&mut self, pane: usize, window: &mut Window, cx: &mut Context<Self>) {
        let showing_this = self.changes.open && self.panes.focused() == pane;
        self.focus_pane(pane, window, cx);
        self.set_changes_open(!showing_this, window, cx);
    }

    fn save_changes_layout(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let changes = self.changes;
        self.persist(window, cx, move |store| ops::save_changes(store, &changes));
    }

    fn device_width_on_screen(&self) -> f32 {
        if self.device.open {
            self.device.width
        } else {
            0.
        }
    }

    fn changes_width_on_screen(&self) -> f32 {
        if self.changes.open {
            self.changes.width
        } else {
            0.
        }
    }

    fn resize_changes(&mut self, width: f32, window: &mut Window, cx: &mut Context<Self>) {
        let max = (window.viewport_size().width.as_f32()
            - SIDEBAR_WIDTH
            - MIN_PANES_WIDTH
            - self.device_width_on_screen())
        .max(CHANGES_WIDTH_RANGE.0);
        let Some(width) = changes_width(width.min(max)) else {
            return;
        };
        if (width - self.changes.width).abs() < 0.5 {
            return;
        }
        self.changes.width = width;
        cx.notify();
        // Dropping the previous task cancels it: only the last width is written.
        self.save_changes = Some(cx.spawn_in(window, async move |this, cx| {
            cx.background_executor().timer(SAVE_DEBOUNCE).await;
            let _ = this.update_in(cx, |this, window, cx| this.save_changes_layout(window, cx));
        }));
    }

    fn set_device_open(&mut self, open: bool, window: &mut Window, cx: &mut Context<Self>) {
        if self.device.open != open {
            self.device.open = open;
            tracing::info!(open, "device panel");
            self.save_device_layout(window, cx);
            cx.notify();
        }
        self.device_panel
            .update(cx, |panel, cx| panel.set_visible(open, window, cx));
    }

    fn toggle_device(&mut self, _: &ToggleDevice, window: &mut Window, cx: &mut Context<Self>) {
        self.set_device_open(!self.device.open, window, cx);
    }

    fn toggle_device_stats(
        &mut self,
        _: &ToggleDeviceStats,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.device_panel.update(cx, |panel, cx| {
            let stats = !panel.stats();
            panel.set_stats(stats, cx);
        });
    }

    fn save_device_layout(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let device = self.device.clone();
        self.persist(window, cx, move |store| ops::save_device(store, &device));
    }

    fn resize_device(&mut self, width: f32, window: &mut Window, cx: &mut Context<Self>) {
        let max = (window.viewport_size().width.as_f32()
            - SIDEBAR_WIDTH
            - MIN_PANES_WIDTH
            - self.changes_width_on_screen())
        .max(DEVICE_WIDTH_RANGE.0);
        let Some(width) = device_width(width.min(max)) else {
            return;
        };
        if (width - self.device.width).abs() < 0.5 {
            return;
        }
        self.device.width = width;
        cx.notify();
        self.save_device = Some(cx.spawn_in(window, async move |this, cx| {
            cx.background_executor().timer(SAVE_DEBOUNCE).await;
            let _ = this.update_in(cx, |this, window, cx| this.save_device_layout(window, cx));
        }));
    }

    fn save_agent(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let agent = self.agent.to_string();
        self.persist(window, cx, move |store| ops::save_agent(store, &agent));
    }

    fn on_loaded(
        &mut self,
        loaded: anyhow::Result<(Arc<Store>, Loaded)>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.loading = false;
        match loaded {
            Ok((store, loaded)) => {
                self.store = Some(store);
                self.projects = ProjectList::new(loaded.entries);
                if let Some(layout) = loaded.layout {
                    self.restore_layout(&layout, window, cx);
                }
                if let Some(changes) = loaded.changes {
                    self.changes = changes.sanitized();
                }
                if let Some(device) = loaded.device {
                    self.device = device.sanitized();
                    let (selected, stats) = (self.device.selected.clone(), self.device.stats);
                    let open = self.device.open;
                    let logs = self.device.logs_settings();
                    self.device_panel.update(cx, |panel, cx| {
                        panel.restore_logs(logs);
                        panel.restore(selected, stats, cx);
                        if open {
                            panel.set_visible(true, window, cx);
                        }
                    });
                }
                self.sync_diffs(cx);
                if let Some(agent) = loaded.agent.filter(|a| agents::preset(a).is_some()) {
                    self.agent = agent.into();
                    self.agent_chosen_by_user = true;
                    let agent = self.agent.clone();
                    self.agent_select.update(cx, |select, cx| {
                        select.set_selected_value(&agent, window, cx);
                    });
                }
                if let Some(err) = loaded.settings_error {
                    self.report(&err, window, cx);
                }
            }
            Err(err) => {
                self.load_error = Some(format!("{err:#}").into());
                self.report(&err, window, cx);
            }
        }
        cx.notify();
    }

    /// Shows the saved workspaces (without starting agents) and re-applies the split once the
    /// pane group has been laid out.
    fn restore_layout(&mut self, layout: &PaneLayout, window: &mut Window, cx: &mut Context<Self>) {
        let projects = &self.projects;
        self.panes = Panes::from_layout(layout, |id| projects.is_active(id));
        self.split = sanitize_split(layout.split);
        let Some(split) = self.split else {
            return;
        };
        cx.spawn_in(window, async move |this, cx| {
            for _ in 0..40 {
                let applied = this
                    .update_in(cx, |this, window, cx| {
                        this.pane_sizes.update(cx, |sizes, cx| {
                            let total = sizes.container_size();
                            if sizes.sizes().len() < PANE_COUNT || total.as_f32() <= 0. {
                                return false;
                            }
                            sizes.resize_panel(0, total * split, window, cx);
                            tracing::debug!(split, sizes = ?sizes.sizes(), "pane split restored");
                            true
                        })
                    })
                    .unwrap_or(true);
                if applied {
                    return;
                }
                cx.background_executor()
                    .timer(Duration::from_millis(50))
                    .await;
            }
            tracing::warn!("pane split not restored: panes were never laid out");
        })
        .detach();
    }

    fn on_path(&mut self, path: ResolvedPath, window: &mut Window, cx: &mut Context<Self>) {
        if let PathSource::Process(reason) = &path.source {
            window.push_notification(
                Notification::warning(format!(
                    "Could not read PATH from the login shell; agents may not be found. {reason}"
                )),
                cx,
            );
        }
        let mut gh = hangar_worktree::Gh::new().with_path_env(path.value.clone());
        if let Some(program) = std::env::var_os("HANGAR_GH").filter(|p| !p.is_empty()) {
            gh = gh.with_program(program);
        }
        self.changes_panel
            .update(cx, |panel, cx| panel.set_gh(gh, cx));
        // find_program stats files; cheap enough for five presets but keep it off this thread.
        let value = path.value.clone();
        self.path = Some(path);
        cx.spawn_in(window, async move |this, cx| {
            let choices = cx
                .background_executor()
                .spawn(async move { agents::choices(Some(&value)) })
                .await;
            let _ = this.update_in(cx, |this, window, cx| {
                this.set_agent_choices(choices, window, cx)
            });
        })
        .detach();
    }

    fn set_agent_choices(
        &mut self,
        choices: Vec<AgentChoice>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self.agent_chosen_by_user {
            self.agent = agents::default_agent(&choices).into();
        }
        let installed: Vec<&str> = choices
            .iter()
            .filter(|c| c.installed)
            .map(|c| c.id.as_str())
            .collect();
        tracing::info!(?installed, agent = %self.agent, "detected agents");
        let agent = self.agent.clone();
        self.agent_select.update(cx, |select, cx| {
            select.set_items(agent_options(&choices), window, cx);
            select.set_selected_value(&agent, window, cx);
        });
        cx.notify();
    }

    fn pick_project(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let picked = cx.prompt_for_paths(PathPromptOptions {
            files: false,
            directories: true,
            multiple: false,
            prompt: Some("Add Project".into()),
        });
        cx.spawn_in(window, async move |this, cx| {
            let path = match picked.await {
                Ok(Ok(Some(paths))) => paths.into_iter().next(),
                Ok(Ok(None)) | Err(_) => None,
                Ok(Err(err)) => {
                    let _ = this.update_in(cx, |this, window, cx| this.report(&err, window, cx));
                    None
                }
            };
            if let Some(path) = path {
                let _ = this.update_in(cx, |this, window, cx| {
                    this.add_project(path, window, cx).detach();
                });
            }
        })
        .detach();
    }

    fn add_project(
        &mut self,
        path: PathBuf,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Task<Option<Project>> {
        let Some(store) = self.store.clone() else {
            self.report(&anyhow!("the database is not open"), window, cx);
            return Task::ready(None);
        };
        cx.spawn_in(window, async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn(async move { ops::add_project(&store, &path) })
                .await;
            this.update_in(cx, |this, window, cx| match result {
                Ok(project) => {
                    tracing::info!(name = %project.name, repo = %project.repo_path.display(), "project added");
                    this.projects.add_project(project.clone());
                    cx.notify();
                    Some(project)
                }
                Err(err) => {
                    this.report(&err, window, cx);
                    None
                }
            })
            .ok()
            .flatten()
        })
    }

    fn open_new_workspace(
        &mut self,
        project_id: ProjectId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let input = cx.new(|cx| InputState::new(window, cx).placeholder("Workspace name"));
        let submit = cx.subscribe_in(
            &input,
            window,
            move |this, input, event: &InputEvent, window, cx| {
                if !matches!(event, InputEvent::PressEnter { .. }) {
                    return;
                }
                let name = input.read(cx).value().trim().to_owned();
                if name.is_empty() {
                    return;
                }
                this.new_workspace = None;
                let agent = this.agent.to_string();
                this.create_workspace(project_id, name, agent, window, cx)
                    .detach();
                cx.notify();
            },
        );
        input.update(cx, |state, cx| state.focus(window, cx));
        self.new_workspace = Some(NewWorkspaceInput {
            project_id,
            input,
            _submit: submit,
        });
        cx.notify();
    }

    fn close_new_workspace(&mut self, cx: &mut Context<Self>) {
        self.new_workspace = None;
        cx.notify();
    }

    /// Creates the worktree and workspace off the main thread, shows it in the focused pane and
    /// starts `agent_id` in it. Resolves once the agent has started (or failed).
    fn create_workspace(
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
    fn start_session(
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

    fn attach_session(
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

    fn on_session_exit(
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
    fn close_session(
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

    fn archive_workspace(
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

    fn restore_workspace(
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

    fn confirm_delete_workspace(
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

    fn load_delete_summary(
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
    fn delete_workspace(
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

    fn confirm_remove_project(
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

    fn remove_project(
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

    fn select_workspace(
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

    fn focus_pane(&mut self, pane: usize, window: &mut Window, cx: &mut Context<Self>) {
        if self.panes.focused() != pane {
            self.panes.focus(pane);
            self.focus_terminal(window, cx);
            self.panes_changed(window, cx);
            cx.notify();
        }
    }

    fn focus_terminal(&self, window: &mut Window, cx: &mut Context<Self>) {
        let live = self
            .panes
            .slot(self.panes.focused())
            .and_then(|id| self.sessions.get(&id));
        if let Some(live) = live {
            let handle = live.view.focus_handle(cx);
            window.focus(&handle, cx);
        }
    }

    /// The first active workspace with this slug, for dev hooks.
    fn workspace_by_slug(&self, slug: &str) -> Option<WorkspaceId> {
        self.projects
            .entries()
            .iter()
            .flat_map(|e| &e.workspaces)
            .find(|w| w.slug == slug)
            .map(|w| w.id)
    }

    fn render_sidebar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let app = cx.entity().downgrade();
        let focused_workspace = self.panes.slot(self.panes.focused());
        let row = |ws: &Workspace| WorkspaceRow {
            id: ws.id,
            name: ws.name.clone().into(),
            status: if self.busy.contains(&ws.id) {
                Some("…".into())
            } else {
                self.sessions
                    .get(&ws.id)
                    .map(|live| short_status(&live.session.status).into())
            },
            changes: self
                .diffs
                .get(&ws.id)
                .and_then(|entry| entry.diff.read(cx).badge())
                .map(SharedString::from),
            active: focused_workspace == Some(ws.id),
            busy: self.busy.contains(&ws.id),
        };
        let sections = self.projects.entries().iter().map(|entry| {
            let project_id = entry.project.id;
            ProjectSection {
                app: app.clone(),
                project_id,
                name: entry.project.name.clone().into(),
                workspaces: entry.active().map(row).collect(),
                archived: entry.archived().map(row).collect(),
                input: self
                    .new_workspace
                    .as_ref()
                    .filter(|n| n.project_id == project_id)
                    .map(|n| n.input.clone()),
                creating: self.creating.contains(&project_id),
                collapsed: false,
            }
        });

        let device_open = self.device.open;
        let header = h_flex()
            .w_full()
            .gap_1()
            .items_center()
            .child(div().font_semibold().child("Hangar"))
            .child(div().flex_1())
            .child(
                Button::new("device-toggle")
                    .ghost()
                    .small()
                    .icon(if device_open {
                        IconName::PanelRightClose
                    } else {
                        IconName::PanelRightOpen
                    })
                    .label("Device")
                    .selected(device_open)
                    .tooltip(if device_open {
                        "Hide device (⌘⇧P)"
                    } else {
                        "Show device (⌘⇧P)"
                    })
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.set_device_open(!device_open, window, cx)
                    })),
            )
            .child(
                Button::new("add-project")
                    .ghost()
                    .small()
                    .icon(IconName::FolderOpen)
                    .label("Add project")
                    .on_click(cx.listener(|this, _, window, cx| this.pick_project(window, cx))),
            );

        let notice = if self.loading {
            Some(SharedString::from("Loading…"))
        } else if let Some(err) = &self.load_error {
            Some(err.clone())
        } else if self.projects.is_empty() {
            Some("No projects yet. Add a Git repository to start.".into())
        } else {
            None
        };

        let footer = v_flex()
            .w_full()
            .gap_1()
            .when_some(notice, |this, notice| {
                let color = if self.load_error.is_some() {
                    cx.theme().danger
                } else {
                    cx.theme().muted_foreground
                };
                this.child(div().text_xs().text_color(color).child(notice))
            })
            .child(
                div()
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .child("Agent for new workspaces"),
            )
            .child(Select::new(&self.agent_select).small());

        Sidebar::new("projects")
            .collapsible(SidebarCollapsible::None)
            .w(px(SIDEBAR_WIDTH))
            .header(header)
            .children(sections)
            .footer(footer)
    }

    fn render_pane(&self, pane: usize, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let focused = self.panes.focused() == pane;
        let workspace_id = self.panes.slot(pane);
        let workspace = workspace_id.and_then(|id| self.projects.workspace(id));
        let live = workspace_id.and_then(|id| self.sessions.get(&id));

        let header = h_flex()
            .h_8()
            .px_2()
            .gap_3()
            .flex_shrink_0()
            .items_center()
            .text_xs()
            .border_b_1()
            .border_color(theme.border)
            .when(focused, |this| this.bg(theme.secondary))
            .text_color(theme.muted_foreground);
        let changes_open = self.changes.open && focused;
        let toggle = Button::new(("changes-toggle", pane))
            .ghost()
            .xsmall()
            .icon(if changes_open {
                IconName::PanelRightClose
            } else {
                IconName::PanelRightOpen
            })
            .tooltip(if changes_open {
                "Hide changes (⌘⇧D)"
            } else {
                "Show changes (⌘⇧D)"
            })
            .on_click(
                cx.listener(move |this, _, window, cx| this.toggle_changes_for(pane, window, cx)),
            );
        let header = match workspace {
            None => header.child("Empty pane").child(div().flex_1()),
            Some(ws) => {
                let (agent, status, color) = match live {
                    Some(live) => (
                        live.agent_name.clone(),
                        status_label(&live.session.status),
                        match live.session.status {
                            SessionStatus::Running | SessionStatus::Starting => theme.success,
                            SessionStatus::Exited { code: 0 } => theme.muted_foreground,
                            _ => theme.danger,
                        },
                    ),
                    None => (
                        "no agent".into(),
                        "not running".to_owned(),
                        theme.muted_foreground,
                    ),
                };
                let can_start = !self.busy.contains(&ws.id)
                    && live.is_none_or(|l| l.session.status.is_terminal());
                let ws_id = ws.id;
                let label = if live.is_some() { "Restart" } else { "Start" };
                header
                    .child(
                        div()
                            .font_semibold()
                            .text_color(theme.foreground)
                            .child(ws.name.clone()),
                    )
                    .child(ws.branch.clone())
                    .child(agent)
                    .child(div().text_color(color).child(status))
                    .child(div().flex_1())
                    .when(can_start, |this| {
                        this.child(
                            Button::new(("start", pane))
                                .ghost()
                                .xsmall()
                                .icon(IconName::Play)
                                .label(format!("{label} {}", self.agent_display_name()))
                                .on_click(cx.listener(move |this, _, window, cx| {
                                    let agent = this.agent.to_string();
                                    this.panes.show(ws_id);
                                    this.panes_changed(window, cx);
                                    this.start_session(ws_id, agent, window, cx).detach();
                                })),
                        )
                    })
            }
        };
        let header = header.child(toggle);

        let body = div().flex_1().min_h_0().w_full();
        let body = match live {
            Some(live) => body.child(live.view.clone()),
            None => body
                .flex()
                .items_center()
                .justify_center()
                .text_sm()
                .text_color(theme.muted_foreground)
                .child(if workspace.is_some() {
                    "No agent running in this workspace."
                } else {
                    "Select a workspace in the sidebar to show it here."
                }),
        };

        v_flex()
            .id(("pane", pane))
            .size_full()
            .border_1()
            .border_color(if focused { theme.ring } else { theme.border })
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, _: &MouseDownEvent, window, cx| {
                    this.focus_pane(pane, window, cx);
                }),
            )
            .child(header)
            .child(body)
    }

    fn agent_display_name(&self) -> String {
        agents::preset(&self.agent)
            .map(|a| a.display_name)
            .unwrap_or_else(|| self.agent.to_string())
    }

    fn dev_open(
        &mut self,
        repo: PathBuf,
        dev: DevOptions,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Task<Option<()>> {
        tracing::info!(repo = %repo.display(), "HANGAR_DEV_OPEN");
        let add = self.add_project(ops::canonical(&repo), window, cx);
        cx.spawn_in(window, async move |this, cx| {
            let project = add.await?;
            let create = this
                .update_in(cx, |this, window, cx| {
                    this.create_workspace(project.id, dev.workspace, dev.agent, window, cx)
                })
                .ok()?;
            let workspace_id = create.await?;
            if let Some(input) = dev.input {
                this.update(cx, |this, _| {
                    let Some(live) = this.sessions.get(&workspace_id) else {
                        return;
                    };
                    if let Err(err) = live.handle.session().write(format!("{input}\r")) {
                        tracing::error!(%err, "HANGAR_DEV_INPUT: write failed");
                    }
                })
                .ok()?;
            }
            Some(())
        })
    }
}

/// Runs the `HANGAR_DEV_*` hooks in order: open, archive, restore, delete, remove project.
async fn run_dev_hooks(this: WeakEntity<HangarApp>, dev: DevOptions, cx: &mut AsyncWindowContext) {
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
    if let Some(repo) = dev.open.clone() {
        let open = dev.clone();
        if let Ok(task) =
            this.update_in(cx, |this, window, cx| this.dev_open(repo, open, window, cx))
        {
            task.await;
        }
    }
    run_review_hooks(&this, &dev, cx).await;
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

impl Render for HangarApp {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let mut panes = h_resizable("panes").with_state(&self.pane_sizes);
        for pane in 0..PANE_COUNT {
            panes = panes.child(resizable_panel().child(self.render_pane(pane, cx)));
        }
        let changes = self.changes.open.then(|| {
            div()
                .relative()
                .h_full()
                .w(px(self.changes.width))
                .flex_shrink_0()
                .border_l_1()
                .border_color(cx.theme().border)
                .child(self.changes_panel.clone())
                .child(
                    div()
                        .id("changes-resize")
                        .absolute()
                        .left(px(-3.))
                        .top_0()
                        .h_full()
                        .w(px(6.))
                        .cursor_col_resize()
                        .on_drag(ChangesResize, |_, _, _, cx| cx.new(|_| ChangesResize)),
                )
        });
        let device = self.device.open.then(|| {
            div()
                .relative()
                .h_full()
                .w(px(self.device.width))
                .flex_shrink_0()
                .border_l_1()
                .border_color(cx.theme().border)
                .child(self.device_panel.clone())
                .child(
                    div()
                        .id("device-resize")
                        .absolute()
                        .left(px(-3.))
                        .top_0()
                        .h_full()
                        .w(px(6.))
                        .cursor_col_resize()
                        .on_drag(DeviceResize, |_, _, _, cx| cx.new(|_| DeviceResize)),
                )
        });
        h_flex()
            .id("hangar")
            .key_context("Hangar")
            .track_focus(&self.focus)
            .on_action(cx.listener(Self::toggle_changes))
            .on_action(cx.listener(Self::toggle_device))
            .on_action(cx.listener(Self::toggle_device_stats))
            .on_drag_move(
                cx.listener(|this, event: &DragMoveEvent<ChangesResize>, window, cx| {
                    // The device panel, when open, is right of the changes panel.
                    let right = event.bounds.right() - px(this.device_width_on_screen());
                    let width = right - event.event.position.x;
                    this.resize_changes(width.as_f32(), window, cx);
                }),
            )
            .on_drag_move(
                cx.listener(|this, event: &DragMoveEvent<DeviceResize>, window, cx| {
                    let width = event.bounds.right() - event.event.position.x;
                    this.resize_device(width.as_f32(), window, cx);
                }),
            )
            .size_full()
            .bg(cx.theme().background)
            .text_color(cx.theme().foreground)
            .child(self.render_sidebar(cx))
            .child(div().flex_1().min_w_0().h_full().child(panes))
            .children(changes)
            .children(device)
    }
}

fn agent_options(choices: &[AgentChoice]) -> Vec<AgentOption> {
    choices
        .iter()
        .map(|c| AgentOption {
            id: c.id.clone().into(),
            title: if c.installed {
                c.name.clone().into()
            } else {
                format!("{} (not found)", c.name).into()
            },
        })
        .collect()
}

fn short_status(status: &SessionStatus) -> String {
    match status {
        SessionStatus::Failed { .. } => "failed".to_owned(),
        other => status_label(other),
    }
}
