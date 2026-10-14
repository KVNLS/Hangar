//! What a workspace's agent is doing, as the UI shows it (sidebar glyph, overview groups,
//! status bar counts).

use crate::SessionStatus;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum AgentState {
    /// Paused on a tool call that needs the user's OK. Only structured agents (ACP, M7) report it.
    Approval,
    /// Asked a question and is blocked on a reply. Only structured agents (ACP, M7) report it.
    Input,
    /// The agent process failed or exited with an error.
    Failed,
    /// Finished with changes waiting for review.
    Review,
    /// Producing output or running tools.
    Working,
    /// Stopped, quiet, or never started.
    Idle,
}

impl AgentState {
    /// "Needs you" order: approval → input → failed → review → working → idle.
    pub const ORDER: [AgentState; 6] = [
        Self::Approval,
        Self::Input,
        Self::Failed,
        Self::Review,
        Self::Working,
        Self::Idle,
    ];

    /// Derives the state of a PTY agent. `recently_active` means the terminal produced output
    /// within the activity window; `has_changes` means the worktree differs from its base.
    pub fn from_pty(
        status: Option<&SessionStatus>,
        recently_active: bool,
        has_changes: bool,
    ) -> Self {
        match status {
            Some(SessionStatus::Failed { .. }) => Self::Failed,
            Some(SessionStatus::Exited { code }) if *code != 0 => Self::Failed,
            Some(SessionStatus::Exited { .. }) if has_changes => Self::Review,
            Some(SessionStatus::Starting) => Self::Working,
            Some(SessionStatus::Running) if recently_active => Self::Working,
            _ => Self::Idle,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Approval => "Waiting for approval",
            Self::Input => "Needs input",
            Self::Failed => "Failed",
            Self::Review => "Ready for review",
            Self::Working => "Working",
            Self::Idle => "Idle",
        }
    }

    pub fn short_label(self) -> &'static str {
        match self {
            Self::Approval => "approval",
            Self::Input => "input",
            Self::Failed => "failed",
            Self::Review => "review",
            Self::Working => "working",
            Self::Idle => "idle",
        }
    }

    /// Blocked on the user (approval, input or failure).
    pub fn needs_you(self) -> bool {
        matches!(self, Self::Approval | Self::Input | Self::Failed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn order_matches_derive_ord() {
        let mut sorted = AgentState::ORDER;
        sorted.sort();
        assert_eq!(sorted, AgentState::ORDER);
    }

    #[test]
    fn pty_mapping() {
        use AgentState::*;
        let running = SessionStatus::Running;
        let ok = SessionStatus::Exited { code: 0 };
        let err = SessionStatus::Exited { code: 2 };
        let failed = SessionStatus::Failed {
            message: "spawn".into(),
        };
        assert_eq!(AgentState::from_pty(None, false, true), Idle);
        assert_eq!(
            AgentState::from_pty(Some(&SessionStatus::Starting), false, false),
            Working
        );
        assert_eq!(AgentState::from_pty(Some(&running), true, false), Working);
        assert_eq!(AgentState::from_pty(Some(&running), false, true), Idle);
        assert_eq!(AgentState::from_pty(Some(&ok), false, true), Review);
        assert_eq!(AgentState::from_pty(Some(&ok), false, false), Idle);
        assert_eq!(AgentState::from_pty(Some(&err), false, true), Failed);
        assert_eq!(AgentState::from_pty(Some(&failed), true, false), Failed);
    }
}
