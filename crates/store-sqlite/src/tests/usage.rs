use super::*;

fn usage_entry(session_id: SessionId, at: i64, model: &str, amount: f64) -> UsageEntry {
    UsageEntry {
        session_id,
        at: Timestamp(at),
        model: Some(model.to_owned()),
        local: model.starts_with("ollama/"),
        cost: Some(Cost {
            amount,
            currency: "USD".into(),
        }),
        tokens: Some(TokenCount {
            input: 1_200,
            output: 300,
            cache_read: 50_000,
            cache_write: 0,
        }),
    }
}

#[test]
fn usage_rows_round_trip_from_a_time_on() {
    let store = store();
    let p = project("/repos/usage");
    store.insert_project(&p).unwrap();
    let w = workspace(p.id, "usage");
    store.insert_workspace(&w).unwrap();
    let s = Session::new(w.id, AgentKind::Claude);
    store.insert_session(&s).unwrap();
    let old = usage_entry(s.id, 1_000, "claude-sonnet-4-5", 0.1);
    let later = usage_entry(s.id, 3_000, "claude-opus-4-1", 0.4);
    let local = UsageEntry {
        cost: None,
        tokens: None,
        ..usage_entry(s.id, 2_000, "ollama/qwen3-coder:30b", 0.0)
    };
    store.record_usage(&later).unwrap();
    store.record_usage(&old).unwrap();
    store.record_usage(&local).unwrap();
    assert_eq!(
        store.list_usage_since(Timestamp(0)).unwrap(),
        vec![old, local.clone(), later.clone()]
    );
    assert_eq!(
        store.list_usage_since(Timestamp(2_000)).unwrap(),
        vec![local, later]
    );
    assert!(
        store
            .record_usage(&usage_entry(SessionId::new(), 1, "x", 0.0))
            .is_err()
    );
}

#[test]
fn usage_outlives_deleted_workspaces_and_projects() {
    let store = store();
    let p = project("/repos/demo");
    store.insert_project(&p).unwrap();
    let w = workspace(p.id, "Gone");
    store.insert_workspace(&w).unwrap();
    let s = Session::new(w.id, AgentKind::Claude);
    store.insert_session(&s).unwrap();
    store
        .record_usage(&usage_entry(s.id, 10, "claude-opus-5", 2.0))
        .unwrap();
    store.delete_workspace(w.id).unwrap();
    store.delete_project(p.id).unwrap();
    let usage = store.list_usage_since(Timestamp(0)).unwrap();
    assert_eq!(usage, vec![usage_entry(s.id, 10, "claude-opus-5", 2.0)]);
}
