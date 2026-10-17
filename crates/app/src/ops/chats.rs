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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ops::fixtures::fixture;
    use crate::ops::{SavedChat, load, save_workspace_agent};
    use hangar_core::changes::{Cost, UsageEntry};
    use hangar_core::{Session, Setting};
    use hangar_usecases::usage::{Period, UsageSettings};

    #[test]
    fn a_deleted_workspace_forgets_its_chat_and_agent() {
        let f = fixture();
        let ws = f.workspace("chat");
        let chat = SavedChat {
            agent: "claude-acp".into(),
            session: "s-1".into(),
        };
        save_chat(&f.store, ws.id, &chat).unwrap();
        save_workspace_agent(&f.store, ws.id, "claude-acp").unwrap();
        let loaded = load(&f.store).unwrap();
        assert_eq!(loaded.chats.get(&ws.id), Some(&chat));
        assert_eq!(
            loaded.workspace_agents.get(&ws.id).map(String::as_str),
            Some("claude-acp")
        );

        forget_chat(&f.store, ws.id).unwrap();
        let loaded = load(&f.store).unwrap();
        assert!(!loaded.chats.contains_key(&ws.id));
        assert!(!loaded.workspace_agents.contains_key(&ws.id));
    }

    #[test]
    fn session_cost_and_usage_are_recorded() {
        let f = fixture();
        let ws = f.workspace("usage");
        let session = Session::new(ws.id, crate::agents::kind("shell"));
        f.store.insert_session(&session).unwrap();
        let session = session.id;
        let cost = Cost {
            amount: 1.25,
            currency: "USD".into(),
        };
        save_session_cost(&f.store, session, &cost).unwrap();
        assert_eq!(
            f.store.list_sessions(ws.id).unwrap()[0].cost,
            Some(cost.clone())
        );

        let since = Timestamp::now();
        let row = |model: &str| UsageEntry {
            session_id: session,
            at: since,
            model: Some(model.into()),
            local: false,
            cost: Some(cost.clone()),
            tokens: None,
        };
        record_usage(&f.store, &[row("a"), row("b")]).unwrap();
        let (settings, entries) = usage_since(&f.store, since, true).unwrap();
        assert_eq!(settings, None);
        assert_eq!(entries, vec![row("a"), row("b")]);
    }

    #[test]
    fn usage_settings_round_trip_and_bad_ones_fall_back() {
        let f = fixture();
        let settings = UsageSettings {
            period: Period::Month,
            reference_model: Some("claude-sonnet-4-5".into()),
            prices: Vec::new(),
        };
        save_usage_settings(&f.store, &settings).unwrap();
        let since = Timestamp::now();
        assert_eq!(
            usage_since(&f.store, since, true).unwrap().0,
            Some(settings)
        );
        assert_eq!(usage_since(&f.store, since, false).unwrap().0, None);

        f.store.set_setting(&Setting::new("usage"), &42).unwrap();
        let (settings, entries) = usage_since(&f.store, since, true).unwrap();
        assert_eq!(settings, None);
        assert!(entries.is_empty());
    }
}
