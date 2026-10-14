//! Every value Hangar saves in the settings table, with the type stored under its key. Keys
//! are part of the saved data: renaming one loses what users saved under it.

use hangar_core::{Setting, WorkspaceId};

use crate::device_panel::{DeviceKey, Platform};
use crate::device_pool::policy::{Assignment, Limit};
use crate::ops::{OllamaSettings, SavedChat};
use crate::shell::RunConfig;
use crate::state::{ChangesLayout, DeviceLayout, PaneLayout};
use crate::ui::theme::Appearance;

pub const LAYOUT: Setting<PaneLayout> = Setting::new("layout");
/// The agent picked for new workspaces.
pub const AGENT: Setting<String> = Setting::new("agent");
pub const CHANGES: Setting<ChangesLayout> = Setting::new("changes_panel");
pub const DEVICE: Setting<DeviceLayout> = Setting::new("device_panel");
pub const APPEARANCE: Setting<Appearance> = Setting::new("appearance");
pub const OLLAMA: Setting<OllamaSettings> = Setting::new("ollama");

pub const DEVICE_LIMIT: Setting<Limit> = Setting::new("device_pool:limit");
pub const SINGLE_DEVICE_ALERT_SHOWN: Setting<bool> = Setting::new("device_pool:single_alert_shown");
pub const LAST_DEVICE_TARGET: Setting<DeviceKey> = Setting::new("device_pool:last_target");

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
        assert_eq!(DEVICE_LIMIT.key(), "device_pool:limit");
        assert_eq!(
            SINGLE_DEVICE_ALERT_SHOWN.key(),
            "device_pool:single_alert_shown"
        );
        assert_eq!(LAST_DEVICE_TARGET.key(), "device_pool:last_target");
        assert_eq!(chat(ws).key(), format!("chat:{ws}"));
        assert_eq!(agent_config("codex").key(), "agent_config:codex");
        assert_eq!(run(ws).key(), format!("run:{ws}"));
        assert_eq!(device_assignment(ws).key(), format!("device:{ws}"));
        assert_eq!(
            device_base(Platform::Android).key(),
            "device_pool:base:android"
        );
    }
}
