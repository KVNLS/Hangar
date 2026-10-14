//! What structured sessions save (the conversation to resume, its cost) and the usage ledger.

use anyhow::Context as _;
use hangar_core::changes::{Cost, UsageEntry};
use hangar_core::ports::Store;
use hangar_core::{SessionId, Timestamp, WorkspaceId};
use hangar_usecases::settings::SettingsExt as _;
use hangar_usecases::usage::UsageSettings;

use super::SavedChat;
use crate::setting_keys;

pub fn save_chat(
    store: &dyn Store,
    workspace: WorkspaceId,
    chat: &SavedChat,
) -> anyhow::Result<()> {
    store
        .set_setting(&setting_keys::chat(workspace), chat)
        .context("failed to save the conversation to resume")
}

/// Forgets the workspace's saved conversation and agent (delete). Both are tried; the first
/// failure is returned.
pub fn forget_chat(store: &dyn Store, workspace: WorkspaceId) -> anyhow::Result<()> {
    let chat = store
        .delete_setting(&setting_keys::chat(workspace))
        .context("failed to forget the saved conversation");
    let agent = store
        .delete_setting(&setting_keys::workspace_agent(workspace))
        .context("failed to forget the workspace's agent");
    chat.and(agent)
}

/// Records what the session has cost so far.
pub fn save_session_cost(store: &dyn Store, session: SessionId, cost: &Cost) -> anyhow::Result<()> {
    store
        .record_session_cost(session, cost)
        .context("failed to save the session's cost")
}

/// Appends `rows` to the usage ledger, stopping at the first failure.
pub fn record_usage(store: &dyn Store, rows: &[UsageEntry]) -> anyhow::Result<()> {
    rows.iter()
        .try_for_each(|row| store.record_usage(row).context("failed to save usage"))
}

/// Ledger rows at or after `since`, with the saved usage settings when `with_settings`.
/// Unreadable settings fall back to the defaults (`None`); the history still loads.
pub fn usage_since(
    store: &dyn Store,
    since: Timestamp,
    with_settings: bool,
) -> anyhow::Result<(Option<UsageSettings>, Vec<UsageEntry>)> {
    let settings = if with_settings {
        store
            .get_setting(&setting_keys::USAGE)
            .unwrap_or_else(|err| {
                tracing::warn!("usage settings unreadable, using defaults: {err:#}");
                None
            })
    } else {
        None
    };
    let entries = store
        .list_usage_since(since)
        .context("failed to read the usage history")?;
    Ok((settings, entries))
}

pub fn save_usage_settings(store: &dyn Store, settings: &UsageSettings) -> anyhow::Result<()> {
    store
        .set_setting(&setting_keys::USAGE, settings)
        .context("failed to save the usage settings")
}
