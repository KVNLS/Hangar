//! The main window: project sidebar and tiled terminal panes (one by default, split on demand).

mod agent_choices;
mod approvals;
mod attention;
mod chrome;
mod compare;
mod database;
mod dev;
mod device_api;
mod dialogs;
mod layout;
mod leftovers;
mod lifecycle;
mod mcp;
mod new_workspace;
mod ollama;
mod orchestration;
mod overview;
mod palette;
mod projects;
mod questions;
mod queue;
mod run;
mod settings;
mod setup;
mod sidebar;
mod startup;
mod state;
mod status;
mod toast;
mod types;
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

use crate::agents::{self, AgentChoice, AgentPreset};
use crate::changes::review::ReviewTarget;
use crate::changes::{
    ChangesEvent, ChangesPanel, ExitReview, PanelSource, WorkspaceAgent, WorkspaceDiff,
};
use crate::chat_view::{ChatEvent, ChatView};
use crate::device_panel::{
    DevicePanel, DevicePanelEvent, KEY_CONTEXT as DEVICE_PANEL, PressHome as DeviceHome,
    Screenshot as DeviceScreenshot,
};
use crate::device_pool::{DevicePool, NoticeLevel, PoolEvent, WorkspaceRef};
use crate::files_view::{ExitFiles, FilesEvent, FilesPanel};
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
use database::open_and_load;
pub use dev::DevOptions;
use dev::run_dev_hooks;
pub use device_api::{DeviceFrame, DeviceStatusInfo};
use dialogs::DeleteWorkspaceDialog;
use state::*;
use types::*;

/// How long a killed agent gets to exit before its worktree is removed.
const KILL_GRACE: Duration = Duration::from_secs(3);
/// How long quitting waits for terminal agents to exit after their hang-up before killing them.
const QUIT_GRACE: Duration = Duration::from_millis(500);
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
        OpenFiles,
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
        KeyBinding::new("cmd-shift-e", OpenFiles, None),
        KeyBinding::new("cmd-r", Run, None),
        KeyBinding::new("cmd-.", StopRun, None),
        // The last binding is the one menus and the palette show.
        KeyBinding::new("cmd-\\", SplitPane, None),
        KeyBinding::new("cmd-d", SplitPane, None),
        // macOS reports ⇧⌘\ as ⌘| (shift folded into the character).
        KeyBinding::new("cmd-|", SplitPaneDown, None),
        KeyBinding::new("cmd-alt-1", FocusPane1, None),
        KeyBinding::new("cmd-alt-2", FocusPane2, None),
        KeyBinding::new("cmd-alt-3", FocusPane3, None),
        KeyBinding::new("cmd-alt-4", FocusPane4, None),
        KeyBinding::new("cmd-w", ClosePane, None),
        KeyBinding::new("cmd-s", DeviceScreenshot, Some(DEVICE_PANEL)),
        KeyBinding::new("cmd-shift-h", DeviceHome, Some(DEVICE_PANEL)),
        KeyBinding::new("escape", ExitReview, Some("ReviewScreen")),
        KeyBinding::new("escape", ExitFiles, Some("FilesScreen")),
        KeyBinding::new("escape", CloseScreen, Some("OverviewScreen")),
        KeyBinding::new("escape", CloseScreen, Some("SettingsScreen")),
        KeyBinding::new("cmd-enter", SubmitSheet, Some("NewWorkspaceSheet")),
        KeyBinding::new("escape", CloseSheet, Some("NewWorkspaceSheet")),
        KeyBinding::new("escape", CloseSheet, Some("CompareAttempts")),
        KeyBinding::new("cmd-enter", SubmitSheet, Some("AnswerSheet")),
        KeyBinding::new("escape", CloseSheet, Some("AnswerSheet")),
    ]);
}

pub struct HangarApp {
    store: Option<Arc<dyn Store>>,
    changes_panel: Entity<ChangesPanel>,
    files_panel: Entity<FilesPanel>,
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
    setup: setup::SetupState,
    live: LiveState,
    work: WorkState,
    toast: Option<toast::Toast>,
    settings_ui: settings::SettingsUi,
    attention: attention::Attention<WorkspaceId>,
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
    fn report(&mut self, err: &anyhow::Error, window: &mut Window, cx: &mut Context<Self>) {
        tracing::error!("{err:#}");
        let mut notification = Notification::error(format!("{err:#}"));
        if setup::is_missing_tool(err) {
            let shell = cx.entity().downgrade();
            notification = notification.action(move |_, _, _| {
                let shell = shell.clone();
                gpui_kit::component::button::Button::new("open-setup")
                    .label("Open Setup")
                    .on_click(move |_, window, cx| {
                        let _ = shell.update(cx, |this, cx| this.open_setup(window, cx));
                    })
            });
        }
        window.push_notification(notification, cx);
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
        let src = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
        let module = std::fs::read_to_string(src.join("shell.rs")).unwrap();
        let mut sources = vec![module.clone()];
        let mut dirs = vec![src.join("shell")];
        while let Some(dir) = dirs.pop() {
            for path in std::fs::read_dir(dir).unwrap().flatten().map(|e| e.path()) {
                if path.is_dir() {
                    dirs.push(path);
                } else {
                    sources.push(std::fs::read_to_string(path).unwrap());
                }
            }
        }
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
