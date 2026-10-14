//! Agent presets as the app sees them: which are installed and which one is the default.

use std::ffi::OsStr;

use hangar_agents::{PtyAdapter, find_program, presets};
use hangar_core::AgentKind;

pub const DEFAULT_AGENT: &str = "claude";
pub const FALLBACK_AGENT: &str = "shell";

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AgentChoice {
    pub id: String,
    pub name: String,
    pub installed: bool,
}

/// Presets with their install state, resolved against `path` (a `PATH`-style list).
pub fn choices(path: Option<&str>) -> Vec<AgentChoice> {
    presets()
        .into_iter()
        .map(|adapter| AgentChoice {
            installed: is_installed(&adapter, path),
            id: adapter.id,
            name: adapter.display_name,
        })
        .collect()
}

pub fn is_installed(adapter: &PtyAdapter, path: Option<&str>) -> bool {
    find_program(&adapter.program, path.map(OsStr::new)).is_some()
}

/// Claude Code if installed, else the shell.
pub fn default_agent(choices: &[AgentChoice]) -> &'static str {
    let claude = choices.iter().any(|c| c.id == DEFAULT_AGENT && c.installed);
    if claude {
        DEFAULT_AGENT
    } else {
        FALLBACK_AGENT
    }
}

pub fn preset(id: &str) -> Option<PtyAdapter> {
    presets().into_iter().find(|adapter| adapter.id == id)
}

pub fn kind(id: &str) -> AgentKind {
    match id {
        "claude" => AgentKind::Claude,
        "codex" => AgentKind::Codex,
        "opencode" => AgentKind::OpenCode,
        "gemini" => AgentKind::Gemini,
        "shell" => AgentKind::Shell,
        other => AgentKind::Custom(other.to_owned()),
    }
}

#[cfg(test)]
mod tests {
    use super::{AgentChoice, choices, default_agent, is_installed, kind, preset};
    use hangar_agents::PtyAdapter;
    use hangar_core::AgentKind;

    fn choice(id: &str, installed: bool) -> AgentChoice {
        AgentChoice {
            id: id.into(),
            name: id.into(),
            installed,
        }
    }

    #[test]
    fn default_prefers_installed_claude() {
        assert_eq!(
            default_agent(&[choice("claude", true), choice("shell", true)]),
            "claude"
        );
        assert_eq!(
            default_agent(&[choice("claude", false), choice("shell", true)]),
            "shell"
        );
        assert_eq!(default_agent(&[]), "shell");
    }

    #[test]
    fn installed_check_uses_given_path() {
        let sh = PtyAdapter::new("sh", "sh", "sh");
        assert!(is_installed(&sh, Some("/nonexistent:/bin")));
        assert!(!is_installed(&sh, Some("/nonexistent")));
        assert!(!is_installed(&sh, None));
    }

    #[test]
    fn presets_map_to_kinds() {
        let all = choices(Some("/bin:/usr/bin"));
        assert!(all.iter().any(|c| c.id == "shell"));
        for c in &all {
            assert!(preset(&c.id).is_some());
            assert!(!matches!(kind(&c.id), AgentKind::Custom(_)), "{}", c.id);
        }
        assert_eq!(kind("aider"), AgentKind::Custom("aider".into()));
        assert!(preset("nope").is_none());
    }
}
