//! Hangar's approval levels and how each maps onto an agent's session modes.
//!
//! No agent has a mode that approves reads but asks for everything else, so "Auto-approve
//! reads" keeps the agent's asking mode and the client answers read-kind permission requests
//! itself ([`auto_allow`]).

use super::protocol::{PermissionKind, PermissionOption, SessionMode};

/// How much a structured agent may do without asking.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ApprovalLevel {
    #[default]
    Ask,
    AutoApproveReads,
    FullAuto,
}

/// Mode ids of the agents Hangar knows, most fitting first. Claude Code (claude-agent-acp
/// 0.85): default "Manual", acceptEdits, plan, auto, bypassPermissions. Codex (codex-acp 0.16):
/// read-only, auto "Default", full-access. Gemini CLI 0.62: default, autoEdit, yolo, plan.
/// OpenCode 1.18 offers build and plan only: neither asks nor bypasses its own permissions.
const ASKING_IDS: &[&str] = &["default", "read-only"];
const FULL_IDS: &[&str] = &["bypassPermissions", "full-access", "yolo"];

/// Matched against lowercased mode names and descriptions of agents Hangar does not know.
const ASKING_WORDS: &[&str] = &[
    "always ask",
    "ask before",
    "ask every time",
    "prompts for approval",
    "asks for approval",
];
/// Whole names of asking modes ("Default" is left out: Codex's Default edits without asking).
const ASKING_NAMES: &[&str] = &["manual", "ask"];
const FULL_WORDS: &[&str] = &[
    "bypass",
    "full access",
    "yolo",
    "accepts all permissions",
    "auto-approves all",
    "without asking",
    "without approval",
];

/// Tool kinds whose permission requests [`ApprovalLevel::AutoApproveReads`] answers. `fetch`
/// is left out: a URL can carry data out of the workspace.
const READ_KINDS: &[&str] = &["read", "search"];

impl ApprovalLevel {
    pub const ALL: [ApprovalLevel; 3] = [Self::Ask, Self::AutoApproveReads, Self::FullAuto];

    /// Stable id, saved in settings.
    pub fn id(self) -> &'static str {
        match self {
            Self::Ask => "ask",
            Self::AutoApproveReads => "auto_reads",
            Self::FullAuto => "full_auto",
        }
    }

    pub fn from_id(id: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|level| level.id() == id)
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Ask => "Ask every time",
            Self::AutoApproveReads => "Auto-approve reads",
            Self::FullAuto => "Full auto",
        }
    }

    /// The agent's mode for this level, or `None` when no offered mode fits (the agent keeps
    /// its own default). Known mode ids win over names and descriptions.
    pub fn mode(self, modes: &[SessionMode]) -> Option<&SessionMode> {
        let (ids, words, names) = match self {
            Self::Ask | Self::AutoApproveReads => (ASKING_IDS, ASKING_WORDS, ASKING_NAMES),
            Self::FullAuto => (FULL_IDS, FULL_WORDS, &[][..]),
        };
        ids.iter()
            .find_map(|id| modes.iter().find(|m| m.id == *id))
            .or_else(|| {
                modes.iter().find(|m| {
                    let name = m.name.to_lowercase();
                    let description = m.description.as_deref().unwrap_or("").to_lowercase();
                    names.contains(&name.as_str())
                        || words
                            .iter()
                            .any(|w| name.contains(w) || description.contains(w))
                })
            })
    }
}

/// The option to answer a permission request with on the user's behalf: allow once, for
/// read-kind tools, when the level approves reads. `None` leaves the request to the user.
pub fn auto_allow<'a>(
    level: ApprovalLevel,
    tool_kind: Option<&str>,
    options: &'a [PermissionOption],
) -> Option<&'a str> {
    if level == ApprovalLevel::Ask || !tool_kind.is_some_and(|k| READ_KINDS.contains(&k)) {
        return None;
    }
    options
        .iter()
        .find(|o| o.kind == PermissionKind::AllowOnce)
        .map(|o| o.id.as_str())
}

#[cfg(test)]
mod tests;
