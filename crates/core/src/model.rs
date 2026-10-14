use std::fmt;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::changes::{Cost, DiffStat};

macro_rules! id_type {
    ($(#[$meta:meta])* $name:ident) => {
        $(#[$meta])*
        #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
        #[serde(transparent)]
        pub struct $name(pub Uuid);

        impl $name {
            /// Generates a new random (v4) id.
            pub fn new() -> Self {
                Self(Uuid::new_v4())
            }
        }

        impl Default for $name {
            fn default() -> Self {
                Self::new()
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                self.0.fmt(f)
            }
        }
    };
}

id_type!(
    /// Identifies a [`Project`].
    ProjectId
);
id_type!(
    /// Identifies a [`Workspace`].
    WorkspaceId
);
id_type!(
    /// Identifies a [`Session`].
    SessionId
);
id_type!(
    /// Links workspaces made from one task for several agents (best of N), to compare them.
    WorkspaceGroupId
);

/// Milliseconds since the Unix epoch.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Timestamp(pub i64);

impl Timestamp {
    pub fn now() -> Self {
        let millis = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| i64::try_from(d.as_millis()).unwrap_or(i64::MAX))
            .unwrap_or_default();
        Self(millis)
    }

    pub fn as_millis(self) -> i64 {
        self.0
    }
}

/// A Git repository registered in Hangar.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Project {
    pub id: ProjectId,
    pub name: String,
    pub repo_path: PathBuf,
    /// Branch new workspaces are based on, e.g. `main`.
    pub default_base: String,
    pub created_at: Timestamp,
}

impl Project {
    pub fn new(
        name: impl Into<String>,
        repo_path: impl Into<PathBuf>,
        default_base: impl Into<String>,
    ) -> Self {
        Self {
            id: ProjectId::new(),
            name: name.into(),
            repo_path: repo_path.into(),
            default_base: default_base.into(),
            created_at: Timestamp::now(),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkspaceStatus {
    Active,
    Archived,
}

impl WorkspaceStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Active => "active",
            Self::Archived => "archived",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "active" => Some(Self::Active),
            "archived" => Some(Self::Archived),
            _ => None,
        }
    }
}

/// A task inside a project, backed by its own Git worktree and branch.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Workspace {
    pub id: WorkspaceId,
    pub project_id: ProjectId,
    pub name: String,
    pub slug: String,
    pub branch: String,
    pub base: String,
    pub worktree_path: PathBuf,
    pub status: WorkspaceStatus,
    pub created_at: Timestamp,
    /// What the agent was asked to do (the New workspace task), if anything.
    #[serde(default)]
    pub task: Option<String>,
    /// The attempts this workspace was created with, if it is one of several.
    #[serde(default)]
    pub group: Option<WorkspaceGroupId>,
    /// The workspace whose agent created this one through the orchestration tools.
    #[serde(default)]
    pub parent: Option<WorkspaceId>,
}

impl Workspace {
    /// The task's first line, else the workspace name: what review, commit and PR titles start from.
    pub fn title(&self) -> &str {
        self.task
            .as_deref()
            .and_then(|task| task.lines().map(str::trim).find(|line| !line.is_empty()))
            .unwrap_or(&self.name)
    }
}

/// A coding agent (or plain shell) that can run in a workspace.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentKind {
    Claude,
    Codex,
    OpenCode,
    Gemini,
    Shell,
    Custom(String),
}

impl AgentKind {
    /// Default program to launch for this agent. `Shell` resolves `$SHELL`.
    pub fn program(&self) -> String {
        match self {
            Self::Claude => "claude".to_owned(),
            Self::Codex => "codex".to_owned(),
            Self::OpenCode => "opencode".to_owned(),
            Self::Gemini => "gemini".to_owned(),
            Self::Shell => std::env::var("SHELL")
                .ok()
                .filter(|shell| !shell.is_empty())
                .unwrap_or_else(|| "/bin/sh".to_owned()),
            Self::Custom(program) => program.clone(),
        }
    }

    /// Neutral two-character mark shown instead of vendor logos.
    pub fn monogram(&self) -> &'static str {
        match self {
            Self::Claude => "CC",
            Self::Codex => "CX",
            Self::OpenCode => "OC",
            Self::Gemini => "GM",
            Self::Shell | Self::Custom(_) => ">_",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum SessionStatus {
    Starting,
    Running,
    Exited { code: i32 },
    Failed { message: String },
}

impl SessionStatus {
    pub fn is_terminal(&self) -> bool {
        matches!(self, Self::Exited { .. } | Self::Failed { .. })
    }
}

/// One agent process run inside a workspace.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Session {
    pub id: SessionId,
    pub workspace_id: WorkspaceId,
    pub agent: AgentKind,
    pub command: String,
    pub args: Vec<String>,
    pub status: SessionStatus,
    pub created_at: Timestamp,
    pub ended_at: Option<Timestamp>,
    /// The first input the agent was given (the workspace task), if any.
    #[serde(default)]
    pub prompt: Option<String>,
    /// The workspace's diff when the session ended; `None` while running or when unknown.
    #[serde(default)]
    pub changes: Option<DiffStat>,
    /// What the session cost so far, when its agent reports it.
    #[serde(default)]
    pub cost: Option<Cost>,
}

impl Session {
    /// A new `Starting` session running the agent's default program.
    pub fn new(workspace_id: WorkspaceId, agent: AgentKind) -> Self {
        Self {
            id: SessionId::new(),
            workspace_id,
            command: agent.program(),
            agent,
            args: Vec::new(),
            status: SessionStatus::Starting,
            created_at: Timestamp::now(),
            ended_at: None,
            prompt: None,
            changes: None,
            cost: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_are_unique_and_serialize_as_plain_uuid() {
        let a = ProjectId::new();
        assert_ne!(a, ProjectId::new());
        let json = serde_json::to_string(&a).unwrap();
        assert_eq!(json, format!("\"{}\"", a.0));
    }

    #[test]
    fn title_is_the_tasks_first_line_else_the_name() {
        let mut ws = Workspace {
            id: WorkspaceId::new(),
            project_id: ProjectId::new(),
            name: "Login".into(),
            slug: "login".into(),
            branch: "hangar/login".into(),
            base: "main".into(),
            worktree_path: PathBuf::from("/wt/login"),
            status: WorkspaceStatus::Active,
            created_at: Timestamp::now(),
            task: None,
            group: None,
            parent: None,
        };
        assert_eq!(ws.title(), "Login");
        ws.task = Some("\n  Fix the login crash \nwith a test".into());
        assert_eq!(ws.title(), "Fix the login crash");
        ws.task = Some(" \n ".into());
        assert_eq!(ws.title(), "Login");
    }

    #[test]
    fn agent_programs() {
        assert_eq!(AgentKind::Claude.program(), "claude");
        assert_eq!(AgentKind::Codex.program(), "codex");
        assert_eq!(AgentKind::OpenCode.program(), "opencode");
        assert_eq!(AgentKind::Gemini.program(), "gemini");
        assert_eq!(AgentKind::Custom("aider".into()).program(), "aider");
        assert!(!AgentKind::Shell.program().is_empty());
    }

    #[test]
    fn session_status_round_trips_through_json() {
        for status in [
            SessionStatus::Starting,
            SessionStatus::Running,
            SessionStatus::Exited { code: 3 },
            SessionStatus::Failed {
                message: "boom".into(),
            },
        ] {
            let json = serde_json::to_string(&status).unwrap();
            assert_eq!(
                serde_json::from_str::<SessionStatus>(&json).unwrap(),
                status
            );
        }
        assert!(SessionStatus::Exited { code: 0 }.is_terminal());
        assert!(!SessionStatus::Running.is_terminal());
    }

    #[test]
    fn workspace_status_str_round_trip() {
        for status in [WorkspaceStatus::Active, WorkspaceStatus::Archived] {
            assert_eq!(WorkspaceStatus::parse(status.as_str()), Some(status));
        }
        assert_eq!(WorkspaceStatus::parse("nope"), None);
    }

    #[test]
    fn timestamp_now_is_after_2020() {
        assert!(Timestamp::now().as_millis() > 1_577_836_800_000);
    }
}
