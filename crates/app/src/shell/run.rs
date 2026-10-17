//! Run loop (⌘R): build a workspace and run it on its own device (from the device pool). The
//! commands come from `hangar_runner` and run in terminal tabs next to the agent (a dev server
//! tab for Metro, a build tab); installs and launches go through the device.

mod exit;
mod flow;
mod launch;
mod prepare;

use std::path::{Path, PathBuf};

use anyhow::Context as _;
use hangar_agents::{AgentAdapter as _, PtyAdapter, SpawnContext};
use hangar_core::SessionStatus;
use hangar_runner::{RunPlan, Stack, Step};
use tokio::sync::oneshot;

use super::toast::ToastKind;
use super::*;
use crate::device_pool::Lease;
use crate::device_view::DeviceHandle;
use crate::ops::RunConfig;
use prepare::{Prepared, prepare, spawn_command};

/// Expo prints this once the app is open on the device and the dev server is attached.
const EXPO_LAUNCHED: &str = "Logs for your project will appear below";
/// How far back the launch watcher looks for [`EXPO_LAUNCHED`].
const LAUNCH_SCAN_LINES: usize = 2_000;
/// Lines of build output quoted when a build fails.
const FAILURE_LINES: usize = 25;

/// Who waits for a run to be launched, and the device lease it holds until then: the build and
/// the install keep the device, the running app does not (the workspace keeps an ordinary
/// reservation that expires idle, so other agents get the device while the app runs).
struct Completion {
    done: oneshot::Sender<anyhow::Result<String>>,
    lease: Lease,
}

/// Which terminal a pane shows for its workspace.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(super) enum PaneTab {
    #[default]
    Agent,
    Server,
    Build,
}

/// What the user must pick before a run can be planned.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ChoiceKind {
    /// Several apps build for the device.
    Stack,
    /// The Xcode container has several schemes and none is named after it.
    Scheme,
}

/// `prepare` cannot go on without the user's pick: `options` are (saved value, label).
#[derive(Debug)]
struct NeedsChoice {
    kind: ChoiceKind,
    options: Vec<(String, String)>,
}

impl std::fmt::Display for NeedsChoice {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let what = match self.kind {
            ChoiceKind::Stack => "app to run",
            ChoiceKind::Scheme => "scheme to build",
        };
        write!(f, "pick the {what} (")?;
        let labels: Vec<&str> = self.options.iter().map(|(_, l)| l.as_str()).collect();
        write!(f, "{}), then run again", labels.join(", "))
    }
}

impl std::error::Error for NeedsChoice {}

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
