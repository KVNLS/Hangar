//! Every value Hangar saves in the settings table, with the type stored under its key. Keys
//! are part of the saved data: renaming one loses what users saved under it.

use hangar_core::{Setting, WorkspaceId};
use hangar_usecases::agents::StartRequest;
use hangar_usecases::usage::UsageSettings;

use crate::device_panel::{DeviceKey, Platform};
use crate::device_pool::policy::{Assignment, Limit};
use crate::ops::{OllamaSettings, RunConfig, SavedChat, SavedMode};
use crate::state::{ChangesLayout, DeviceLayout, PaneLayout};
use crate::ui::theme::Appearance;

pub const LAYOUT: Setting<PaneLayout> = Setting::new("layout");
/// The agent picked for new workspaces.
pub const AGENT: Setting<String> = Setting::new("agent");
pub const CHANGES: Setting<ChangesLayout> = Setting::new("changes_panel");
pub const DEVICE: Setting<DeviceLayout> = Setting::new("device_panel");
pub const APPEARANCE: Setting<Appearance> = Setting::new("appearance");
pub const OLLAMA: Setting<OllamaSettings> = Setting::new("ollama");
/// Settings › Usage: period shown, reference model for savings, custom prices.
pub const USAGE: Setting<UsageSettings> = Setting::new("usage");
/// Whether commits from the review box credit the workspace's agent with a trailer.
pub const COAUTHOR_TRAILER: Setting<bool> = Setting::new("review:coauthor_trailer");
/// Dock badge, bounce and system notification when an agent waits on approval, input or a
/// failure (default on).
pub const NOTIFY_ATTENTION: Setting<bool> = Setting::new("permissions:notify_attention");
/// System notification when an agent finishes a turn (default on).
pub const NOTIFY_FINISHED: Setting<bool> = Setting::new("permissions:notify_finished");
/// Default approval level of structured agents, as an `ApprovalLevel` id; missing means
/// "Ask every time".
pub const APPROVAL_LEVEL: Setting<String> = Setting::new("permissions:approval_level");
/// "Max concurrent agents": starts beyond it queue (see [`hangar_usecases::agents::AgentQueue`]).
pub const MAX_AGENTS: Setting<u32> = Setting::new("agents:max_concurrent");
/// Starts waiting for a slot, first to start first; they start again after a relaunch.
pub const AGENT_QUEUE: Setting<Vec<(WorkspaceId, StartRequest)>> = Setting::new("agents:queue");
/// "Let agents create and drive workspaces": the MCP orchestration tools (default off).
pub const ORCHESTRATION: Setting<bool> = Setting::new("agents:orchestration");

/// Terminals send Option+key as Meta (ESC-prefixed) instead of composing characters
/// (default off).
pub const OPTION_AS_META: Setting<bool> = Setting::new("terminal:option_as_meta");
/// Programs in a terminal may write the clipboard (OSC 52) (default off).
pub const CLIPBOARD_WRITE: Setting<bool> = Setting::new("terminal:clipboard_write");

pub const DEVICE_LIMIT: Setting<Limit> = Setting::new("device_pool:limit");
pub const SINGLE_DEVICE_ALERT_SHOWN: Setting<bool> = Setting::new("device_pool:single_alert_shown");
pub const LAST_DEVICE_TARGET: Setting<DeviceKey> = Setting::new("device_pool:last_target");

/// The agent preset a workspace last started; Resume, Retry and Start run it again there.
pub fn workspace_agent(id: WorkspaceId) -> Setting<String> {
    Setting::scoped("workspace_agent", id)
}

/// The structured conversation a workspace can resume.
pub fn chat(id: WorkspaceId) -> Setting<SavedChat> {
    Setting::scoped("chat", id)
}

/// Model, effort and other non-mode choices made in an agent's chat, as `(config id, value)`
/// pairs in the order they were chosen. The mode is left out: it decides what the agent may do
/// without asking.
pub fn agent_config(agent: &str) -> Setting<Vec<(String, String)>> {
    Setting::scoped("agent_config", agent)
}

/// The session mode picked in a workspace's composer; it wins over [`APPROVAL_LEVEL`] when
/// the same agent starts there again.
pub fn workspace_mode(id: WorkspaceId) -> Setting<SavedMode> {
    Setting::scoped("mode", id)
}

/// A workspace's dev-server port and Xcode scheme.
pub fn run(id: WorkspaceId) -> Setting<RunConfig> {
    Setting::scoped("run", id)
}

/// The device a workspace runs on.
pub fn device_assignment(id: WorkspaceId) -> Setting<Assignment> {
    Setting::scoped("device", id)
}

/// The base device new workspaces of `platform` get.
pub fn device_base(platform: Platform) -> Setting<String> {
    Setting::scoped("device_pool:base", platform.id())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Saved data lives under these exact strings.
    #[test]
    fn keys_are_stable() {
        let ws = WorkspaceId::new();
        assert_eq!(LAYOUT.key(), "layout");
        assert_eq!(AGENT.key(), "agent");
        assert_eq!(CHANGES.key(), "changes_panel");
        assert_eq!(DEVICE.key(), "device_panel");
        assert_eq!(APPEARANCE.key(), "appearance");
        assert_eq!(OLLAMA.key(), "ollama");
        assert_eq!(USAGE.key(), "usage");
        assert_eq!(COAUTHOR_TRAILER.key(), "review:coauthor_trailer");
        assert_eq!(NOTIFY_ATTENTION.key(), "permissions:notify_attention");
        assert_eq!(NOTIFY_FINISHED.key(), "permissions:notify_finished");
        assert_eq!(APPROVAL_LEVEL.key(), "permissions:approval_level");
        assert_eq!(workspace_mode(ws).key(), format!("mode:{ws}"));
        assert_eq!(MAX_AGENTS.key(), "agents:max_concurrent");
        assert_eq!(AGENT_QUEUE.key(), "agents:queue");
        assert_eq!(ORCHESTRATION.key(), "agents:orchestration");
        assert_eq!(DEVICE_LIMIT.key(), "device_pool:limit");
        assert_eq!(
            SINGLE_DEVICE_ALERT_SHOWN.key(),
            "device_pool:single_alert_shown"
        );
        assert_eq!(LAST_DEVICE_TARGET.key(), "device_pool:last_target");
        assert_eq!(chat(ws).key(), format!("chat:{ws}"));
        assert_eq!(workspace_agent(ws).key(), format!("workspace_agent:{ws}"));
        assert_eq!(agent_config("codex").key(), "agent_config:codex");
        assert_eq!(run(ws).key(), format!("run:{ws}"));
        assert_eq!(device_assignment(ws).key(), format!("device:{ws}"));
        assert_eq!(
            device_base(Platform::Android).key(),
            "device_pool:base:android"
        );
    }
}
