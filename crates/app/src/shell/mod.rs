//! The main window: project sidebar and two tiled terminal panes.

mod chrome;
mod dev;
mod dialogs;
mod layout;
mod lifecycle;
mod sidebar;
mod status;
mod view;

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use anyhow::anyhow;
use gpui_kit::component::button::ButtonVariant;
use gpui_kit::component::input::{InputEvent, InputState};
use gpui_kit::component::notification::Notification;
use gpui_kit::component::resizable::{ResizablePanelEvent, ResizableState};
use gpui_kit::component::select::{SelectEvent, SelectItem, SelectState};
use gpui_kit::component::{
    ActiveTheme as _, IconName, WindowExt as _, h_flex, h_resizable, resizable_panel, v_flex,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use hangar_agents::AgentHandle;
use hangar_core::{
    AgentState, Project, ProjectId, Session, Store, Workspace, WorkspaceId, WorkspaceStatus,
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
    Panes, ProjectEntry, ProjectList, changes_width, device_width, sanitize_split, split_ratio,
    status_label,
};
use crate::terminal_view::{TerminalEvent, TerminalView, TerminalViewOptions, TerminalWaker};
pub use chrome::titlebar_options;
pub use dev::DevOptions;
use dev::run_dev_hooks;
use dialogs::DeleteWorkspaceDialog;
use view::agent_options;

/// Width of the sidebar; the panes share the rest.
const SIDEBAR_WIDTH: f32 = 260.;
/// How long a killed agent gets to exit before its worktree is removed.
const KILL_GRACE: Duration = Duration::from_secs(3);
/// The panes keep at least this much width when the changes panel is dragged wider.
const MIN_PANES_WIDTH: f32 = 320.;
/// Quiet time after a panel resize before the width is saved.
const SAVE_DEBOUNCE: Duration = Duration::from_millis(400);

actions!(
    hangar,
    [
        ToggleChanges,
        ToggleDevice,
        ToggleDeviceStats,
        ToggleSidebar,
        NewWorkspace,
        FilterWorkspaces,
        AddProject
    ]
);

pub fn init(cx: &mut App) {
    cx.bind_keys([
        KeyBinding::new("cmd-shift-d", ToggleChanges, None),
        KeyBinding::new("cmd-shift-p", ToggleDevice, None),
        KeyBinding::new("cmd-alt-shift-p", ToggleDeviceStats, None),
        KeyBinding::new("cmd-b", ToggleSidebar, None),
        KeyBinding::new("cmd-n", NewWorkspace, None),
        KeyBinding::new("cmd-p", FilterWorkspaces, None),
        KeyBinding::new("cmd-o", AddProject, None),
    ]);
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
    activity: status::Activity,
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
    sidebar_open: bool,
    sidebar_filter: Entity<InputState>,
    collapsed_projects: HashSet<ProjectId>,
    /// Projects whose archived group is expanded.
    show_archived: HashSet<ProjectId>,
    _activity: Task<()>,
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
        let sidebar_filter =
            cx.new(|cx| InputState::new(window, cx).placeholder("Filter workspaces"));
        let filter_sub = cx.subscribe(&sidebar_filter, |_, _, _: &InputEvent, cx| cx.notify());
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
            sidebar_open: true,
            sidebar_filter,
            collapsed_projects: HashSet::new(),
            show_archived: HashSet::new(),
            _activity: Self::track_activity(cx),
            _subscriptions: vec![select_sub, resize_sub, changes_sub, device_sub, filter_sub],
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
}
