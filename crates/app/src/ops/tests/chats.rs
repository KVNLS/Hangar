use super::*;
use hangar_core::changes::{Cost, UsageEntry};
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
    let session = f.store.list_sessions(ws.id).unwrap()[0].id;
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

#[test]
fn coauthor_trailer_round_trips() {
    let f = fixture();
    assert_eq!(coauthor_trailer(&f.store).unwrap(), None);
    save_coauthor_trailer(&f.store, false).unwrap();
    assert_eq!(coauthor_trailer(&f.store).unwrap(), Some(false));
    save_coauthor_trailer(&f.store, true).unwrap();
    assert_eq!(coauthor_trailer(&f.store).unwrap(), Some(true));
}
