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

#[cfg(test)]
mod tests {
    use super::*;
    use hangar_core::Setting;
    use hangar_store_sqlite::Store;

    #[test]
    fn run_config_round_trips_and_defaults() {
        let store = Store::open_in_memory().unwrap();
        let id = WorkspaceId::new();
        assert_eq!(run_config(&store, id), RunConfig::default());

        let config = RunConfig {
            port: Some(8082),
            scheme: Some("App".into()),
            stack: Some("expo".into()),
        };
        save_run_config(&store, id, &config).unwrap();
        assert_eq!(run_config(&store, id), config);
        assert_eq!(run_config(&store, WorkspaceId::new()), RunConfig::default());

        // An unreadable value runs with the defaults.
        let key: Setting<String> = Setting::scoped("run", id);
        store.set_setting(&key, &"nope".to_owned()).unwrap();
        assert_eq!(run_config(&store, id), RunConfig::default());
    }

    #[test]
    fn a_picked_app_forgets_the_scheme() {
        let store = Store::open_in_memory().unwrap();
        let id = WorkspaceId::new();
        let saved = RunConfig {
            port: Some(8081),
            scheme: Some("Old".into()),
            stack: Some("xcode".into()),
        };
        save_run_config(&store, id, &saved).unwrap();

        save_run_choice(&store, id, RunChoice::Scheme("New".into())).unwrap();
        assert_eq!(
            run_config(&store, id),
            RunConfig {
                scheme: Some("New".into()),
                ..saved.clone()
            }
        );

        save_run_choice(&store, id, RunChoice::Stack("expo".into())).unwrap();
        assert_eq!(
            run_config(&store, id),
            RunConfig {
                port: Some(8081),
                scheme: None,
                stack: Some("expo".into()),
            }
        );
    }

    #[test]
    fn forgetting_the_run_config_frees_the_port() {
        let store = Store::open_in_memory().unwrap();
        let id = WorkspaceId::new();
        let config = RunConfig {
            port: Some(8081),
            ..RunConfig::default()
        };
        save_run_config(&store, id, &config).unwrap();
        forget_run_config(&store, id).unwrap();
        assert_eq!(run_config(&store, id), RunConfig::default());
        // Forgetting nothing is fine.
        forget_run_config(&store, id).unwrap();
    }
}
