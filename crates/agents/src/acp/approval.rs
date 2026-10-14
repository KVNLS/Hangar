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
mod tests {
    use super::*;
    use crate::acp::protocol::parse_modes;
    use serde_json::json;

    fn ids(level: ApprovalLevel, modes: &[SessionMode]) -> Option<&str> {
        level.mode(modes).map(|m| m.id.as_str())
    }

    /// `session/new` results recorded from the agents (spikes/m7-acp-notes.md).
    fn claude() -> Vec<SessionMode> {
        parse_modes(&json!({"modes": {"currentModeId": "default", "availableModes": [
            {"id": "default", "name": "Manual", "description": "Always ask before making changes"},
            {"id": "acceptEdits", "name": "Accept edits", "description": "Automatically accept all file edits"},
            {"id": "plan", "name": "Plan", "description": "Create a plan before making changes"},
            {"id": "auto", "name": "Auto", "description": "Claude handles permission decisions"},
            {"id": "bypassPermissions", "name": "Bypass permissions", "description": "Accepts all permissions"}
        ]}}))
        .1
    }

    fn codex() -> Vec<SessionMode> {
        parse_modes(&json!({"modes": {"currentModeId": "read-only", "availableModes": [
            {"id": "read-only", "name": "Read Only", "description": "Codex can read files in the current workspace. Approval is required to edit files or access the internet."},
            {"id": "auto", "name": "Default", "description": "Codex can read and edit files in the current workspace, and run commands. Approval is required to access the internet or edit other files. (Identical to Agent mode)"},
            {"id": "full-access", "name": "Full Access", "description": "Codex can edit files outside this workspace and access the internet without asking for approval. Exercise caution when using."}
        ]}}))
        .1
    }

    fn opencode() -> Vec<SessionMode> {
        parse_modes(&json!({"configOptions": [{"id": "mode", "name": "Session Mode", "category": "mode",
            "type": "select", "currentValue": "build", "options": [
            {"value": "build", "name": "build", "description": "The default agent. Executes tools based on configured permissions."},
            {"value": "plan", "name": "plan", "description": "Plan mode. Disallows all edit tools."}
        ]}]}))
        .1
    }

    /// From Gemini CLI 0.62's `buildAvailableModes` (not signed in here, so not seen live).
    fn gemini() -> Vec<SessionMode> {
        parse_modes(
            &json!({"modes": {"currentModeId": "default", "availableModes": [
                {"id": "default", "name": "Default", "description": "Prompts for approval"},
                {"id": "autoEdit", "name": "Auto Edit", "description": "Auto-approves edit tools"},
                {"id": "yolo", "name": "YOLO", "description": "Auto-approves all tools"},
                {"id": "plan", "name": "Plan", "description": "Read-only mode"}
            ]}}),
        )
        .1
    }

    #[test]
    fn levels_map_onto_known_agents() {
        use ApprovalLevel::*;
        for (modes, ask, full) in [
            (claude(), Some("default"), Some("bypassPermissions")),
            (codex(), Some("read-only"), Some("full-access")),
            (gemini(), Some("default"), Some("yolo")),
            (opencode(), None, None),
        ] {
            assert_eq!(ids(Ask, &modes), ask);
            assert_eq!(ids(AutoApproveReads, &modes), ask);
            assert_eq!(ids(FullAuto, &modes), full);
        }
    }

    #[test]
    fn unknown_agents_match_by_name_or_description() {
        let mode = |id: &str, name: &str, description: Option<&str>| SessionMode {
            id: id.into(),
            name: name.into(),
            description: description.map(Into::into),
        };
        let modes = [
            mode("a", "Careful", Some("Always ask before running tools")),
            mode("b", "Edits", Some("Accept edits")),
            mode("c", "Trust me", Some("Runs everything without asking")),
        ];
        assert_eq!(ids(ApprovalLevel::Ask, &modes), Some("a"));
        assert_eq!(ids(ApprovalLevel::FullAuto, &modes), Some("c"));
        let named = [mode("m", "Manual", None), mode("y", "YOLO", None)];
        assert_eq!(ids(ApprovalLevel::Ask, &named), Some("m"));
        assert_eq!(ids(ApprovalLevel::FullAuto, &named), Some("y"));
        assert_eq!(
            ids(ApprovalLevel::FullAuto, &[mode("b", "Edits", None)]),
            None
        );
        assert_eq!(ids(ApprovalLevel::Ask, &[]), None);
    }

    #[test]
    fn ids_round_trip() {
        for level in ApprovalLevel::ALL {
            assert_eq!(ApprovalLevel::from_id(level.id()), Some(level));
        }
        assert_eq!(ApprovalLevel::from_id("nope"), None);
        assert_eq!(ApprovalLevel::default(), ApprovalLevel::Ask);
    }

    #[test]
    fn reads_are_allowed_once_when_the_level_approves_them() {
        let option = |id: &str, kind| PermissionOption {
            id: id.into(),
            name: id.into(),
            kind,
        };
        let options = [
            option("always", PermissionKind::AllowAlways),
            option("once", PermissionKind::AllowOnce),
            option("no", PermissionKind::RejectOnce),
        ];
        use ApprovalLevel::*;
        assert_eq!(
            auto_allow(AutoApproveReads, Some("read"), &options),
            Some("once")
        );
        assert_eq!(
            auto_allow(AutoApproveReads, Some("search"), &options),
            Some("once")
        );
        assert_eq!(auto_allow(FullAuto, Some("read"), &options), Some("once"));
        assert_eq!(auto_allow(Ask, Some("read"), &options), None);
        for kind in ["edit", "execute", "fetch", "delete", "move", "other"] {
            assert_eq!(
                auto_allow(AutoApproveReads, Some(kind), &options),
                None,
                "{kind}"
            );
        }
        assert_eq!(auto_allow(AutoApproveReads, None, &options), None);
        assert_eq!(
            auto_allow(AutoApproveReads, Some("read"), &options[..1]),
            None
        );
    }
}
