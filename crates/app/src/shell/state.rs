//! The groups of state `HangarApp` holds.

use super::*;

/// How terminals treat Option and programs' clipboard writes (both off by default).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(super) struct TerminalPrefs {
    pub(super) option_as_meta: bool,
    pub(super) clipboard_write: bool,
}

/// What the window shows and how it is laid out, with its debounced saves.
pub(super) struct Layout {
    pub(super) panes: Panes,
    pub(super) pane_groups: layout::PaneGroups,
    /// Pending resize of the pane groups to the panes' shares; replacing it cancels the
    /// previous one.
    pub(super) apply_shares: Option<Task<()>>,
    pub(super) palette: Option<palette::Palette>,
    pub(super) overview_by_project: bool,
    pub(super) settings_tab: settings::SettingsTab,
    pub(super) appearance: crate::ui::theme::Appearance,
    pub(super) _appearance_observer: Subscription,
    pub(super) changes: ChangesLayout,
    pub(super) files_open: bool,
    /// Settings › Appearance › Terminal.
    pub(super) terminal: TerminalPrefs,
    /// The window's device panel; it shows the focused workspace's device.
    pub(super) device: DeviceLayout,
    /// The right panel's width, shared by its tabs.
    pub(super) panel_width: f32,
    /// The tab the right panel shows when reopened.
    pub(super) panel_tab: layout::PanelTab,
    /// The pending (debounced) pane layout save.
    pub(super) save_panes: Option<Task<()>>,
    pub(super) sidebar_open: bool,
    pub(super) sidebar_width: f32,
    pub(super) screen: Screen,
    /// Focused while the overview or settings fill the window, so esc returns to the workspace.
    pub(super) screen_focus: FocusHandle,
    pub(super) sidebar_filter: Entity<InputState>,
    pub(super) collapsed_projects: HashSet<ProjectId>,
    /// Projects whose archived group is expanded.
    pub(super) show_archived: HashSet<ProjectId>,
}

/// Which agent new sessions start, what is installed, and the login PATH they run with.
pub(super) struct AgentSetup {
    pub(super) path: Option<ResolvedPath>,
    pub(super) agent: SharedString,
    pub(super) agent_chosen_by_user: bool,
    /// Agent presets with their install state (all assumed installed until the PATH is known).
    pub(super) agent_choices: Vec<AgentChoice>,
    pub(super) ollama: ollama::OllamaState,
    /// "Let agents create and drive workspaces" (the MCP orchestration tools).
    pub(super) orchestration: bool,
}

/// What runs per workspace: agent sessions, build and dev-server tabs, live diffs.
pub(super) struct LiveState {
    pub(super) sessions: HashMap<WorkspaceId, LiveSession>,
    /// Structured conversations each workspace can resume (saved in settings).
    pub(super) saved_chats: HashMap<WorkspaceId, ops::SavedChat>,
    /// The agent each workspace last started; workspaces without one use the default agent.
    pub(super) workspace_agents: HashMap<WorkspaceId, String>,
    /// Build and dev-server tabs per workspace (⌘R).
    pub(super) runs: HashMap<WorkspaceId, run::WorkspaceRun>,
    /// Dev-server ports picked for runs still preparing, until the run is in `runs`.
    pub(super) port_holds: HashMap<WorkspaceId, u16>,
    /// The tab each workspace's pane shows (agent, dev server or build).
    pub(super) pane_tabs: HashMap<WorkspaceId, run::PaneTab>,
    /// Live diffs of active workspaces, started when first shown in a pane or compared.
    pub(super) diffs: HashMap<WorkspaceId, DiffEntry>,
    /// "Max concurrent agents" and the workspaces waiting for a slot.
    pub(super) agent_queue: hangar_usecases::agents::AgentQueue<queue::StartRequest>,
    /// Questions agents asked the user (`ask_user`), one per workspace.
    pub(super) questions: HashMap<WorkspaceId, questions::PendingQuestion>,
    /// The queue as last saved, to write it only when it changed.
    pub(super) saved_queue: Vec<(WorkspaceId, queue::StartRequest)>,
    /// Agents spawning off the main thread: they hold a slot before they have a session.
    pub(super) starting: HashSet<WorkspaceId>,
    /// Sessions that ended today, for the overview.
    pub(super) done_today: overview::DoneToday,
}

/// Projects and workspaces as loaded, and the operations in flight on them.
pub(super) struct WorkState {
    pub(super) projects: ProjectList,
    pub(super) loading: bool,
    pub(super) load_error: Option<SharedString>,
    pub(super) new_workspace: Option<new_workspace::NewWorkspaceSheet>,
    pub(super) creating: HashSet<ProjectId>,
    /// Workspaces with an archive, restore or delete in flight.
    pub(super) busy: HashSet<WorkspaceId>,
    /// "Compare attempts", when open.
    pub(super) compare: Option<compare::CompareAttempts>,
    /// The answer sheet of an agent's question, when open.
    pub(super) answer: Option<questions::AnswerSheet>,
    /// Child workspaces each agent is creating through the orchestration tools.
    pub(super) child_creates: HashMap<WorkspaceId, usize>,
}
