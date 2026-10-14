//! A workspace's saved run choices (⌘R): dev-server port, Xcode scheme and app.

use anyhow::Context as _;
use hangar_core::WorkspaceId;
use hangar_core::ports::Store;
use hangar_usecases::settings::SettingsExt as _;
use serde::{Deserialize, Serialize};

use crate::setting_keys;

/// Saved per workspace under [`setting_keys::run`].
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RunConfig {
    pub port: Option<u16>,
    pub scheme: Option<String>,
    /// The app picked when the worktree holds several (`Stack::key`).
    #[serde(default)]
    pub stack: Option<String>,
}

/// What the user picked when a run could not go on alone.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RunChoice {
    Stack(String),
    Scheme(String),
}

/// The workspace's saved run config; missing or unreadable is the default.
pub fn run_config(store: &dyn Store, id: WorkspaceId) -> RunConfig {
    store
        .get_setting(&setting_keys::run(id))
        .ok()
        .flatten()
        .unwrap_or_default()
}

pub fn save_run_config(
    store: &dyn Store,
    id: WorkspaceId,
    config: &RunConfig,
) -> anyhow::Result<()> {
    store
        .set_setting(&setting_keys::run(id), config)
        .context("failed to save the run config")
}

/// Records the user's pick in the workspace's run config. Another app has its own schemes, so
/// picking an app forgets the scheme.
pub fn save_run_choice(
    store: &dyn Store,
    id: WorkspaceId,
    choice: RunChoice,
) -> anyhow::Result<()> {
    let mut config = run_config(store, id);
    match choice {
        RunChoice::Stack(stack) => {
            config.stack = Some(stack);
            config.scheme = None;
        }
        RunChoice::Scheme(scheme) => config.scheme = Some(scheme),
    }
    save_run_config(store, id, &config)
}

/// Drops the saved run config, so another workspace can take its port.
pub fn forget_run_config(store: &dyn Store, id: WorkspaceId) -> anyhow::Result<()> {
    store
        .delete_setting(&setting_keys::run(id))
        .context("failed to forget the run config")
}
