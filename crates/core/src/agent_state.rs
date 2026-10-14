//! What a workspace's agent is doing, as the UI shows it (sidebar glyph, overview groups,
//! status bar counts).

use crate::SessionStatus;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum AgentState {
    /// Paused on a tool call that needs the user's OK. Only structured agents (ACP, M7) report it.
    Approval,
    /// Asked a question and is blocked on a reply. Reserved: ACP has no question request, so no
    /// agent reports it yet.
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

/// What a terminal agent last reported through the hooks Hangar gives it (Claude Code). Without
/// hooks a terminal agent's state is guessed from its output.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum AgentSignal {
    /// A prompt was submitted or a tool is running.
    Working,
    /// A permission prompt is shown.
    Approval,
    /// The agent asked the user something.
    Input,
    /// The turn finished; the agent waits for the next prompt.
    TurnEnded,
}

impl AgentSignal {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Working => "working",
            Self::Approval => "approval",
            Self::Input => "input",
            Self::TurnEnded => "turn_ended",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        [Self::Working, Self::Approval, Self::Input, Self::TurnEnded]
            .into_iter()
            .find(|s| s.as_str() == value)
    }
}

/// What a structured agent's current prompt turn is doing.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct AcpTurn {
    pub running: bool,
    /// A tool call waits for the user's permission.
    pub permission_pending: bool,
    /// The last turn ended with an error (not a cancel or a refusal).
    pub failed: bool,
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

    /// Derives the state of a terminal agent that reports `signal` through hooks: waits are
    /// known instead of guessed, and a finished turn with changes is ready for review.
    /// `Working` only holds while the terminal draws (Claude Code animates its spinner for the
    /// whole turn): hooks run as separate processes and one can land after `Stop`.
    pub fn from_signal(
        status: Option<&SessionStatus>,
        signal: AgentSignal,
        recently_active: bool,
        has_changes: bool,
    ) -> Self {
        let signal = match signal {
            AgentSignal::Working if !recently_active => AgentSignal::TurnEnded,
            other => other,
        };
        match status {
            Some(SessionStatus::Failed { .. }) => Self::Failed,
            Some(SessionStatus::Exited { code }) if *code != 0 => Self::Failed,
            Some(SessionStatus::Exited { .. }) if has_changes => Self::Review,
            Some(SessionStatus::Starting) => Self::Working,
            Some(SessionStatus::Running) => match signal {
                AgentSignal::Approval => Self::Approval,
                AgentSignal::Input => Self::Input,
                AgentSignal::Working => Self::Working,
                AgentSignal::TurnEnded if has_changes => Self::Review,
                AgentSignal::TurnEnded => Self::Idle,
            },
            _ => Self::Idle,
        }
    }

    /// Derives the state of a structured (ACP) agent from its process status and current turn.
    pub fn from_acp(status: Option<&SessionStatus>, turn: AcpTurn, has_changes: bool) -> Self {
        match status {
            Some(SessionStatus::Failed { .. }) => Self::Failed,
            Some(SessionStatus::Exited { code }) if *code != 0 => Self::Failed,
            Some(SessionStatus::Starting) => Self::Working,
            Some(SessionStatus::Running) if turn.permission_pending => Self::Approval,
            Some(SessionStatus::Running) if turn.running => Self::Working,
            Some(SessionStatus::Running) if turn.failed => Self::Failed,
            Some(SessionStatus::Running | SessionStatus::Exited { .. }) if has_changes => {
                Self::Review
            }
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
    fn signal_mapping() {
        use AgentSignal as S;
        use AgentState::*;
        let running = Some(&SessionStatus::Running);
        let state = |status, signal, active, changes| {
            AgentState::from_signal(status, signal, active, changes)
        };
        // Waits hold while the terminal is quiet.
        assert_eq!(state(running, S::Approval, false, true), Approval);
        assert_eq!(state(running, S::Input, false, false), Input);
        assert_eq!(state(running, S::Working, true, true), Working);
        assert_eq!(state(running, S::TurnEnded, true, true), Review);
        assert_eq!(state(running, S::TurnEnded, false, false), Idle);
        // A late `Working` with a quiet terminal reads as a finished turn.
        assert_eq!(state(running, S::Working, false, true), Review);
        assert_eq!(state(running, S::Working, false, false), Idle);
        let exited = SessionStatus::Exited { code: 0 };
        assert_eq!(state(Some(&exited), S::Approval, false, false), Idle);
        assert_eq!(state(Some(&exited), S::Approval, false, true), Review);
        let crashed = SessionStatus::Exited { code: 1 };
        assert_eq!(state(Some(&crashed), S::Working, true, false), Failed);
        for s in [S::Working, S::Approval, S::Input, S::TurnEnded] {
            assert_eq!(AgentSignal::parse(s.as_str()), Some(s));
        }
        assert_eq!(AgentSignal::parse("done"), None);
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

    #[test]
    fn acp_mapping() {
        use AgentState::*;
        let running = Some(&SessionStatus::Running);
        let turn = |running, permission_pending, failed| AcpTurn {
            running,
            permission_pending,
            failed,
        };
        assert_eq!(
            AgentState::from_acp(running, turn(true, true, false), false),
            Approval
        );
        assert_eq!(
            AgentState::from_acp(running, turn(true, false, false), true),
            Working
        );
        assert_eq!(
            AgentState::from_acp(running, turn(false, false, true), true),
            Failed
        );
        assert_eq!(
            AgentState::from_acp(running, turn(false, false, false), true),
            Review
        );
        assert_eq!(
            AgentState::from_acp(running, AcpTurn::default(), false),
            Idle
        );
        let crashed = SessionStatus::Exited { code: 1 };
        assert_eq!(
            AgentState::from_acp(Some(&crashed), AcpTurn::default(), true),
            Failed
        );
        assert_eq!(AgentState::from_acp(None, AcpTurn::default(), true), Idle);
    }
}
