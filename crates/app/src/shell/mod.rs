//! The main window: project sidebar and tiled terminal panes (one by default, split on demand).

mod chrome;
mod dev;
mod device_api;
mod dialogs;
mod layout;
mod lifecycle;
mod mcp;
mod new_workspace;
mod ollama;
mod overview;
mod palette;
mod run;
mod settings;
mod sidebar;
mod status;
mod view;
mod welcome;

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use crate::pty::AgentHandle;
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
use hangar_agents::acp::AcpSession;
use hangar_core::ports::Store;
use hangar_core::review::MergeStrategy;
use hangar_core::{
    AgentState, Project, ProjectId, Session, SessionStatus, Workspace, WorkspaceId, WorkspaceStatus,
};
use hangar_terminal::ExitInfo;
use hangar_usecases::settings::SettingsExt as _;

use crate::agents::{self, AgentChoice, AgentPreset};
use crate::changes::review::ReviewTarget;
use crate::changes::{ChangesEvent, ChangesPanel, ExitReview, PanelSource, WorkspaceDiff};
use crate::chat_view::{ChatEvent, ChatView};
use crate::device_panel::{DevicePanel, DevicePanelEvent};
use crate::device_pool::{DevicePool, NoticeLevel, PoolEvent, WorkspaceRef};
use crate::login_env::{self, PathSource, ResolvedPath};
use crate::ops::{self, DeleteOptions, Loaded, StartedSession};
use crate::paths;
use crate::setting_keys;
use crate::state::{
    ChangesLayout, DeviceLayout, MAX_PANES, PANEL_DEFAULT_WIDTH, PANEL_WIDTH_RANGE, PaneLayout,
    Panes, ProjectEntry, ProjectList, SIDEBAR_DEFAULT_WIDTH, pane_shares, panel_width,
    sidebar_width, status_label,
};
use crate::terminal_view::{TerminalEvent, TerminalView, TerminalViewOptions, TerminalWaker};
pub use chrome::titlebar_options;
pub use dev::DevOptions;
use dev::run_dev_hooks;
pub use device_api::{DeviceFrame, DeviceStatusInfo};
use dialogs::DeleteWorkspaceDialog;
pub use run::RunConfig;
use view::agent_options;

/// How long a killed agent gets to exit before its worktree is removed.
const KILL_GRACE: Duration = Duration::from_secs(3);
/// The panes keep at least this much width when the right panel is dragged wider.
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
        AddProject,
        ShowDeviceTab,
        ShowChangesTab,
        ShowFilesTab,
        OpenReview,
        SubmitSheet,
        CloseSheet,
        OpenPalette,
        ToggleTheme,
        ToggleOverview,
        OpenSettings,
        CloseScreen,
        NextNeedingYou,
        Run,
        StopRun,
        RestartServer,
        SplitPane,
        SplitPaneDown,
        ClosePane,
        FocusPane1,
        FocusPane2,
        FocusPane3,
        FocusPane4,
        Quit
    ]
);

pub fn init(cx: &mut App) {
    cx.on_action(|_: &Quit, cx| cx.quit());
    cx.bind_keys([
        KeyBinding::new("cmd-q", Quit, None),
        KeyBinding::new("cmd-shift-d", ToggleChanges, None),
        KeyBinding::new("cmd-shift-p", ToggleDevice, None),
        KeyBinding::new("cmd-alt-shift-p", ToggleDeviceStats, None),
        KeyBinding::new("cmd-b", ToggleSidebar, None),
        KeyBinding::new("cmd-n", NewWorkspace, None),
        KeyBinding::new("cmd-k", OpenPalette, None),
        KeyBinding::new("cmd-shift-o", ToggleOverview, None),
        KeyBinding::new("cmd-j", NextNeedingYou, None),
        KeyBinding::new("cmd-,", OpenSettings, None),
        KeyBinding::new("cmd-p", FilterWorkspaces, None),
        KeyBinding::new("cmd-o", AddProject, None),
        KeyBinding::new("cmd-1", ShowDeviceTab, None),
        KeyBinding::new("cmd-2", ShowChangesTab, None),
        KeyBinding::new("cmd-3", ShowFilesTab, None),
        KeyBinding::new("cmd-shift-r", OpenReview, None),
        KeyBinding::new("cmd-r", Run, None),
        KeyBinding::new("cmd-.", StopRun, None),
        KeyBinding::new("cmd-d", SplitPane, None),
        KeyBinding::new("cmd-\\", SplitPane, None),
        // macOS reports ⌘⇧\ as ⌘| (shift folded into the character).
        KeyBinding::new("cmd-|", SplitPaneDown, None),
        KeyBinding::new("cmd-alt-1", FocusPane1, None),
        KeyBinding::new("cmd-alt-2", FocusPane2, None),
        KeyBinding::new("cmd-alt-3", FocusPane3, None),
        KeyBinding::new("cmd-alt-4", FocusPane4, None),
        KeyBinding::new("cmd-w", ClosePane, None),
        KeyBinding::new("escape", ExitReview, Some("ReviewScreen")),
        KeyBinding::new("escape", CloseScreen, Some("OverviewScreen")),
        KeyBinding::new("escape", CloseScreen, Some("SettingsScreen")),
        KeyBinding::new("cmd-enter", SubmitSheet, Some("NewWorkspaceSheet")),
        KeyBinding::new("escape", CloseSheet, Some("NewWorkspaceSheet")),
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

/// What runs a workspace's agent and how the pane shows it.
enum AgentSurface {
    /// An interactive CLI in a terminal. Dropping the handle kills the program.
    Terminal {
        handle: AgentHandle,
        view: Entity<TerminalView>,
    },
    /// A structured agent over ACP, shown as a chat.
    Chat {
        acp: Arc<AcpSession>,
        view: Entity<ChatView>,
    },
}

struct LiveSession {
    session: Session,
    agent_name: SharedString,
    surface: AgentSurface,
    activity: status::Activity,
    _exit: Subscription,
}

impl LiveSession {
    /// Kills the program if it still runs. Returns the session to record as stopped, if any.
    fn stop(&self) -> Option<hangar_core::SessionId> {
        if self.session.status.is_terminal() {
            return None;
        }
        match &self.surface {
            AgentSurface::Terminal { handle, .. } => {
                if let Err(err) = handle.session().kill() {
                    tracing::warn!(%err, session = %self.session.id, "failed to kill agent");
                }
            }
            AgentSurface::Chat { acp, .. } => acp.kill(),
        }
        Some(self.session.id)
    }

    fn view(&self) -> AnyView {
        match &self.surface {
            AgentSurface::Terminal { view, .. } => view.clone().into(),
            AgentSurface::Chat { view, .. } => view.clone().into(),
        }
    }

    fn focus_handle(&self, cx: &App) -> FocusHandle {
        match &self.surface {
            AgentSurface::Terminal { view, .. } => view.focus_handle(cx),
            AgentSurface::Chat { view, .. } => view.focus_handle(cx),
        }
    }

    /// Types `text` and Return into a terminal agent, or sends it as a chat agent's prompt.
    fn send_text(&self, text: &str, cx: &mut App) -> anyhow::Result<()> {
        match &self.surface {
            AgentSurface::Terminal { handle, .. } => {
                handle.session().write(format!("{text}\r"))?;
                Ok(())
            }
            AgentSurface::Chat { view, .. } => view.update(cx, |chat, cx| chat.send(text, cx)),
        }
    }

    fn exit_waiter(&self) -> ExitWaiter {
        match &self.surface {
            AgentSurface::Terminal { handle, .. } => {
                ExitWaiter::Terminal(Arc::clone(handle.session()))
            }
            AgentSurface::Chat { acp, .. } => ExitWaiter::Acp(Arc::clone(acp)),
        }
    }
}

/// Waits off the main thread for a stopped agent's process to go away.
enum ExitWaiter {
    Terminal(Arc<hangar_terminal::TerminalSession>),
    Acp(Arc<AcpSession>),
}

impl ExitWaiter {
    /// Whether the process ended within `timeout`. Blocks.
    fn wait(&self, timeout: Duration) -> bool {
        match self {
            Self::Terminal(terminal) => terminal.wait_for_exit(timeout).is_some(),
            Self::Acp(acp) => acp.wait_for_exit(timeout),
        }
    }
}

struct DiffEntry {
    diff: Entity<WorkspaceDiff>,
    _observe: Subscription,
}

/// Drag payload of the sidebar's resize handle.
struct SidebarResize;

impl Render for SidebarResize {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        Empty
    }
}

/// Drag payload of the right panel's resize handle.
struct PanelResize;

impl Render for PanelResize {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        Empty
    }
}

/// What fills the window between the title bar and the status bar.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Screen {
    /// Sidebar, panes and the right panel.
    Workspace,
    /// Full-width diff review of the focused workspace (⌘⇧R).
    Review,
    /// Every workspace as a card, grouped by state (⌘⇧O).
    Overview,
    /// Settings (⌘,).
    Settings,
}

/// What the window shows and how it is laid out, with its debounced saves.
struct Layout {
    panes: Panes,
    pane_groups: layout::PaneGroups,
    /// Pending resize of the pane groups to the panes' shares; replacing it cancels the
    /// previous one.
    apply_shares: Option<Task<()>>,
    palette: Option<palette::Palette>,
    overview_by_project: bool,
    settings_tab: settings::SettingsTab,
    appearance: crate::ui::theme::Appearance,
    _appearance_observer: Subscription,
    changes: ChangesLayout,
    files_open: bool,
    /// The window's device panel; it shows the focused workspace's device.
    device: DeviceLayout,
    /// The right panel's width, shared by its tabs.
    panel_width: f32,
    /// The tab the right panel shows when reopened.
    panel_tab: layout::PanelTab,
    /// The pending (debounced) pane layout save.
    save_panes: Option<Task<()>>,
    sidebar_open: bool,
    sidebar_width: f32,
    screen: Screen,
    /// Focused while the overview or settings fill the window, so esc returns to the workspace.
    screen_focus: FocusHandle,
    sidebar_filter: Entity<InputState>,
    collapsed_projects: HashSet<ProjectId>,
    /// Projects whose archived group is expanded.
    show_archived: HashSet<ProjectId>,
}

/// Which agent new sessions start, what is installed, and the login PATH they run with.
struct AgentSetup {
    path: Option<ResolvedPath>,
    agent_select: Entity<AgentSelect>,
    agent: SharedString,
    agent_chosen_by_user: bool,
    /// Agent presets with their install state (all assumed installed until the PATH is known).
    agent_choices: Vec<AgentChoice>,
    ollama: ollama::OllamaState,
}

/// What runs per workspace: agent sessions, build and dev-server tabs, live diffs.
struct LiveState {
    sessions: HashMap<WorkspaceId, LiveSession>,
    /// Structured conversations each workspace can resume (saved in settings).
    saved_chats: HashMap<WorkspaceId, ops::SavedChat>,
    /// Build and dev-server tabs per workspace (⌘R).
    runs: HashMap<WorkspaceId, run::WorkspaceRun>,
    /// The tab each workspace's pane shows (agent, dev server or build).
    pane_tabs: HashMap<WorkspaceId, run::PaneTab>,
    /// Live diffs of active workspaces, started when first shown in a pane.
    diffs: HashMap<WorkspaceId, DiffEntry>,
}

/// Projects and workspaces as loaded, and the operations in flight on them.
struct WorkState {
    projects: ProjectList,
    loading: bool,
    load_error: Option<SharedString>,
    new_workspace: Option<new_workspace::NewWorkspaceSheet>,
    creating: HashSet<ProjectId>,
    /// Workspaces with an archive, restore or delete in flight.
    busy: HashSet<WorkspaceId>,
}

pub struct HangarApp {
    store: Option<Arc<dyn Store>>,
    changes_panel: Entity<ChangesPanel>,
    files_panel: Entity<crate::files_view::FilesPanel>,
    /// Every workspace's device.
    device_pool: Entity<DevicePool>,
    /// Serves the MCP device tools of `Hangar mcp-bridge`; `None` if the socket could not be bound.
    mcp: Option<crate::mcp_host::McpHandle>,
    /// `HANGAR_DEV_DEVICE_LIMIT`, applied when the pool loads.
    device_limit_override: Option<crate::device_pool::policy::Limit>,
    device_panel: Entity<DevicePanel>,
    focus: FocusHandle,
    _activity: Task<()>,
    _subscriptions: Vec<Subscription>,
    layout: Layout,
    agent_setup: AgentSetup,
    live: LiveState,
    work: WorkState,
}

impl HangarApp {
    pub fn new(dev: DevOptions, window: &mut Window, cx: &mut Context<Self>) -> Self {
        // Install state is unknown until the login PATH arrives; list names only for now.
        let initial_choices: Vec<AgentChoice> = agents::all_presets()
            .into_iter()
            .map(|a| AgentChoice {
                id: a.id().to_owned(),
                name: a.display_name().to_owned(),
                installed: true,
            })
            .collect();
        let options = agent_options(&initial_choices);
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
                this.agent_setup.agent = id.clone();
                this.agent_setup.agent_chosen_by_user = true;
                this.save_agent(window, cx);
                cx.notify();
            },
        );
        let pane_groups = Self::new_pane_groups(1, window, cx);

        let changes_panel = cx.new(ChangesPanel::new);
        let changes_sub = cx.subscribe_in(
            &changes_panel,
            window,
            |this, _, event: &ChangesEvent, window, cx| match event {
                ChangesEvent::ExitReview => this.set_screen(Screen::Workspace, window, cx),
            },
        );
        let device_pool = cx.new(DevicePool::new);
        let pool_sub = cx.subscribe_in(
            &device_pool,
            window,
            |_, _, event: &PoolEvent, window, cx| match event {
                PoolEvent::Notice(level, message) => {
                    let notification = match level {
                        NoticeLevel::Info => Notification::info(message.clone()),
                        NoticeLevel::Warning => Notification::warning(message.clone()),
                        NoticeLevel::Error => Notification::error(message.clone()),
                    };
                    window.push_notification(notification, cx);
                }
            },
        );
        let pool_observer = cx.observe(&device_pool, |_, _, cx| cx.notify());
        let device_panel = cx.new(|cx| DevicePanel::new(device_pool.clone(), window, cx));
        let device_sub = cx.subscribe_in(
            &device_panel,
            window,
            |this, _, event: &DevicePanelEvent, window, cx| match event {
                DevicePanelEvent::Selected(key) => {
                    this.layout.device.selected = key.clone();
                    this.save_device_layout(window, cx);
                }
                DevicePanelEvent::Stats(stats) => {
                    this.layout.device.stats = *stats;
                    this.save_device_layout(window, cx);
                }
                DevicePanelEvent::Logs(logs) => {
                    this.layout.device.logs = logs.open;
                    this.layout.device.log_level = Some(logs.min_level.as_str().to_owned());
                    this.save_device_layout(window, cx);
                }
            },
        );
        let sidebar_filter =
            cx.new(|cx| InputState::new(window, cx).placeholder("Filter workspaces"));
        let filter_sub = cx.subscribe(&sidebar_filter, |_, _, _: &InputEvent, cx| cx.notify());
        let appearance_observer = cx.observe_window_appearance(window, |this, window, cx| {
            if this.layout.appearance == crate::ui::theme::Appearance::System {
                this.layout.appearance.apply(window, cx);
            }
        });
        let focus = cx.focus_handle();
        window.focus(&focus, cx);

        let (ollama, ollama_subs) = ollama::OllamaState::new(window, cx);
        let device_limit_override = dev.device_limit;
        let db = paths::db_path();
        cx.spawn_in(window, async move |this, cx| {
            let executor = cx.background_executor().clone();
            let path_task = executor.spawn(async { login_env::resolve() });
            let loaded = executor
                .spawn(async move {
                    let db = db.ok_or_else(|| {
                        anyhow!("cannot locate the database: HOME is not set (set HANGAR_DB)")
                    })?;
                    let store = crate::adapters::open_store(&db)?;
                    let loaded = ops::load(&store)?;
                    tracing::info!(
                        db = %db.display(),
                        projects = loaded.entries.len(),
                        layout = ?loaded.layout,
                        agent = ?loaded.agent,
                        appearance = ?loaded.appearance,
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
            changes_panel,
            files_panel: cx.new(crate::files_view::FilesPanel::new),
            device_pool,
            mcp: mcp::start(window, cx),
            device_limit_override,
            device_panel,
            focus,
            layout: Layout {
                panes: Panes::default(),
                pane_groups,
                apply_shares: None,
                palette: None,
                overview_by_project: false,
                settings_tab: Default::default(),
                appearance: Default::default(),
                _appearance_observer: appearance_observer,
                changes: ChangesLayout::default(),
                files_open: false,
                device: DeviceLayout::default(),
                panel_width: PANEL_DEFAULT_WIDTH,
                panel_tab: Default::default(),
                save_panes: None,
                sidebar_open: true,
                sidebar_width: SIDEBAR_DEFAULT_WIDTH,
                screen: Screen::Workspace,
                screen_focus: cx.focus_handle(),
                sidebar_filter,
                collapsed_projects: HashSet::new(),
                show_archived: HashSet::new(),
            },
            agent_setup: AgentSetup {
                path: None,
                agent_select,
                agent,
                agent_chosen_by_user: false,
                agent_choices: initial_choices,
                ollama,
            },
            live: LiveState {
                sessions: HashMap::new(),
                saved_chats: HashMap::new(),
                runs: HashMap::new(),
                pane_tabs: HashMap::new(),
                diffs: HashMap::new(),
            },
            work: WorkState {
                projects: ProjectList::default(),
                loading: true,
                load_error: None,
                new_workspace: None,
                creating: HashSet::new(),
                busy: HashSet::new(),
            },
            _activity: Self::track_activity(cx),
            _subscriptions: vec![
                // ACP agents run in their own process group and some (codex-acp) outlive a
                // closed stdin, so quitting must stop them like ⌘W does.
                cx.on_app_quit(|this, _| {
                    for live in this.live.sessions.values() {
                        live.stop();
                    }
                    this.flush_layout();
                    async {}
                }),
                select_sub,
                changes_sub,
                device_sub,
                filter_sub,
                pool_sub,
                pool_observer,
            ]
            .into_iter()
            .chain(ollama_subs)
            .collect(),
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
        job: impl FnOnce(&dyn Store) -> anyhow::Result<()> + Send + 'static,
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
        let agent = self.agent_setup.agent.to_string();
        self.persist(window, cx, move |store| ops::save_agent(store, &agent));
    }

    fn on_loaded(
        &mut self,
        loaded: anyhow::Result<(Arc<dyn Store>, Loaded)>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.work.loading = false;
        match loaded {
            Ok((store, loaded)) => {
                self.store = Some(store.clone());
                self.work.projects = ProjectList::new(loaded.entries);
                self.live.saved_chats = loaded.chats;
                let workspaces: Vec<WorkspaceRef> = self
                    .work
                    .projects
                    .entries()
                    .iter()
                    .flat_map(|e| {
                        e.workspaces
                            .iter()
                            .map(move |w| workspace_ref(&e.project, w))
                    })
                    .collect();
                let limit = self.device_limit_override;
                self.device_pool
                    .update(cx, |pool, cx| pool.load(store, workspaces, limit, cx));
                if let Some(layout) = loaded.layout {
                    self.restore_layout(&layout, window, cx);
                }
                if let Some(changes) = loaded.changes {
                    self.layout.changes = changes;
                }
                if let Some(device) = loaded.device {
                    self.layout.device = device;
                    let (selected, stats) = (
                        self.layout.device.selected.clone(),
                        self.layout.device.stats,
                    );
                    let open = self.layout.device.open;
                    let logs = self.layout.device.logs_settings();
                    self.device_panel.update(cx, |panel, cx| {
                        panel.restore_logs(logs);
                        panel.restore(selected, stats, cx);
                        if open {
                            panel.set_visible(true, window, cx);
                        }
                    });
                }
                if let Some(appearance) = loaded.appearance {
                    self.layout.appearance = appearance;
                    appearance.apply(window, cx);
                }
                if let Some(ollama) = loaded.ollama {
                    self.restore_ollama(ollama, window, cx);
                }
                // One right panel, one tab: layouts saved before the merge may have both open.
                if self.layout.device.open {
                    self.layout.changes.open = false;
                }
                if self.layout.changes.open {
                    self.layout.panel_tab = layout::PanelTab::Changes;
                }
                self.sync_diffs(cx);
                self.sync_device_focus(window, cx);
                if let Some(agent) = loaded.agent.filter(|a| agents::preset(a).is_some()) {
                    self.agent_setup.agent = agent.into();
                    self.agent_setup.agent_chosen_by_user = true;
                    let agent = self.agent_setup.agent.clone();
                    self.agent_setup.agent_select.update(cx, |select, cx| {
                        select.set_selected_value(&agent, window, cx);
                    });
                }
                if let Some(err) = loaded.settings_error {
                    self.report(&err, window, cx);
                }
            }
            Err(err) => {
                self.work.load_error = Some(format!("{err:#}").into());
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
        let gh = crate::adapters::code_host(
            path.value.clone(),
            std::env::var_os("HANGAR_GH").filter(|p| !p.is_empty()),
        );
        self.changes_panel
            .update(cx, |panel, cx| panel.set_gh(gh, cx));
        // find_program stats files; cheap enough for five presets but keep it off this thread.
        let value = path.value.clone();
        self.agent_setup.path = Some(path);
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
        if !self.agent_setup.agent_chosen_by_user {
            self.agent_setup.agent = agents::default_agent(&choices).into();
        }
        let installed: Vec<&str> = choices
            .iter()
            .filter(|c| c.installed)
            .map(|c| c.id.as_str())
            .collect();
        tracing::info!(?installed, agent = %self.agent_setup.agent, "detected agents");
        let agent = self.agent_setup.agent.clone();
        let options = agent_options(&choices);
        self.agent_setup.agent_choices = choices;
        self.agent_setup.agent_select.update(cx, |select, cx| {
            select.set_items(options, window, cx);
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
                    this.work.projects.add_project(project.clone());
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

fn workspace_ref(project: &Project, ws: &Workspace) -> WorkspaceRef {
    WorkspaceRef {
        id: ws.id,
        name: ws.name.clone(),
        project: project.name.clone(),
        slug: ws.slug.clone(),
        root: ws.worktree_path.clone(),
    }
}

impl HangarApp {
    /// Tells the pool about the workspace (name, worktree) before it is asked for its device.
    pub(super) fn track_workspace(&mut self, id: WorkspaceId, cx: &mut Context<Self>) {
        let Some(entry) = self
            .work
            .projects
            .entries()
            .iter()
            .find(|e| e.workspaces.iter().any(|w| w.id == id))
        else {
            return;
        };
        let Some(ws) = entry.workspaces.iter().find(|w| w.id == id) else {
            return;
        };
        let r = workspace_ref(&entry.project, ws);
        self.device_pool.update(cx, |pool, _| pool.track(r));
    }

    /// Points the pool and the device panel at the focused pane's workspace.
    pub(super) fn sync_device_focus(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let focused = self
            .layout
            .panes
            .slot(self.layout.panes.focused())
            .filter(|id| self.work.projects.is_active(*id));
        if let Some(id) = focused {
            self.track_workspace(id, cx);
        }
        self.device_pool
            .update(cx, |pool, cx| pool.set_focused(focused, cx));
        self.device_panel
            .update(cx, |panel, cx| panel.set_workspace(focused, window, cx));
    }
}

#[cfg(test)]
mod tests {
    /// A key binding without an `on_action` handler does nothing, silently. Every action
    /// declared for the shell must be taken as `&Action` by some handler in the shell's sources.
    #[test]
    fn every_shell_action_has_a_handler() {
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src/shell");
        let sources: Vec<String> = std::fs::read_dir(&dir)
            .unwrap()
            .flatten()
            .map(|e| std::fs::read_to_string(e.path()).unwrap())
            .collect();
        let module = std::fs::read_to_string(dir.join("mod.rs")).unwrap();
        let start = module.find("actions!(").unwrap();
        let list = &module[start..];
        let list = &list[list.find('[').unwrap() + 1..list.find(']').unwrap()];
        let names: Vec<&str> = list
            .split(',')
            .map(str::trim)
            .filter(|n| !n.is_empty())
            .collect();
        assert!(names.len() > 5, "{names:?}");
        let missing: Vec<&str> = names
            .into_iter()
            .filter(|name| {
                let taken = format!("&{name}");
                !sources.iter().any(|s| {
                    s.match_indices(&taken).any(|(at, _)| {
                        // `&Name` followed by a non-identifier char (not `&NameLonger`).
                        !s[at + taken.len()..]
                            .starts_with(|c: char| c.is_alphanumeric() || c == '_')
                    })
                })
            })
            .collect();
        assert!(missing.is_empty(), "actions without a handler: {missing:?}");
    }
}
