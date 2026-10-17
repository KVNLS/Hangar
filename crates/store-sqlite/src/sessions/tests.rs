use hangar_core::changes::{Cost, DiffStat};
use hangar_core::{AgentKind, Session, SessionId, SessionStatus, Timestamp, WorkspaceId};

use crate::StoreError;
use crate::fixtures::{project, store, workspace};

#[test]
fn session_crud_and_status_updates() {
    let store = store();
    let p = project("/repos/a");
    store.insert_project(&p).unwrap();
    let w = workspace(p.id, "task");
    store.insert_workspace(&w).unwrap();

    let mut s = Session::new(w.id, AgentKind::Custom("aider".into()));
    s.args = vec!["--yes".into(), "--model".into(), "x".into()];
    store.insert_session(&s).unwrap();
    let shell = Session::new(w.id, AgentKind::Shell);
    store.insert_session(&shell).unwrap();
    assert_eq!(store.get_session(s.id).unwrap(), s);
    assert_eq!(store.list_sessions(w.id).unwrap(), vec![s.clone(), shell]);

    let running = store
        .update_session_status(s.id, &SessionStatus::Running)
        .unwrap();
    assert_eq!(running.status, SessionStatus::Running);
    assert_eq!(running.ended_at, None);

    let exited = store
        .update_session_status(s.id, &SessionStatus::Exited { code: 2 })
        .unwrap();
    assert_eq!(exited.status, SessionStatus::Exited { code: 2 });
    let ended_at = exited.ended_at.unwrap();

    let failed = store
        .update_session_status(
            s.id,
            &SessionStatus::Failed {
                message: "late".into(),
            },
        )
        .unwrap();
    assert_eq!(failed.ended_at, Some(ended_at));

    assert!(matches!(
        store.update_session_status(SessionId::new(), &SessionStatus::Running),
        Err(StoreError::NotFound {
            entity: "session",
            ..
        })
    ));
}

#[test]
fn session_for_missing_workspace_is_not_found() {
    let store = store();
    assert!(matches!(
        store.insert_session(&Session::new(WorkspaceId::new(), AgentKind::Claude)),
        Err(StoreError::NotFound {
            entity: "workspace",
            ..
        })
    ));
}

#[test]
fn a_session_keeps_its_prompt_and_the_diff_it_ended_with() {
    let store = store();
    let p = project("/repos/a");
    store.insert_project(&p).unwrap();
    let w = workspace(p.id, "task");
    store.insert_workspace(&w).unwrap();

    let mut s = Session::new(w.id, AgentKind::Codex);
    s.prompt = Some("Fix the login crash".into());
    store.insert_session(&s).unwrap();
    assert_eq!(store.get_session(s.id).unwrap(), s);

    let stat = DiffStat {
        files: 3,
        insertions: 40,
        deletions: 2,
    };
    let done = store
        .finish_session(s.id, &SessionStatus::Exited { code: 0 }, Some(stat))
        .unwrap();
    assert_eq!(done.changes, Some(stat));
    assert!(done.ended_at.is_some());
    assert_eq!(done.prompt, s.prompt);

    // A later status without a diff keeps the recorded one.
    let later = store
        .update_session_status(
            s.id,
            &SessionStatus::Failed {
                message: "x".into(),
            },
        )
        .unwrap();
    assert_eq!(later.changes, Some(stat));
    assert!(matches!(
        store.finish_session(SessionId::new(), &SessionStatus::Running, None),
        Err(StoreError::NotFound { .. })
    ));
}

#[test]
fn sessions_since_span_workspaces_latest_ended_first() {
    let store = store();
    let p = project("/repos/a");
    store.insert_project(&p).unwrap();
    let a = workspace(p.id, "a");
    let b = workspace(p.id, "b");
    store.insert_workspace(&a).unwrap();
    store.insert_workspace(&b).unwrap();
    let ended = |ws: WorkspaceId, at: Option<i64>| {
        let mut s = Session::new(ws, AgentKind::Claude);
        s.ended_at = at.map(Timestamp);
        if at.is_some() {
            s.status = SessionStatus::Exited { code: 0 };
        }
        store.insert_session(&s).unwrap();
        s
    };
    ended(a.id, Some(50));
    let first = ended(a.id, Some(100));
    let second = ended(b.id, Some(200));
    ended(b.id, None);

    let ids: Vec<SessionId> = store
        .list_sessions_since(Timestamp(100))
        .unwrap()
        .into_iter()
        .map(|s| s.id)
        .collect();
    assert_eq!(ids, [second.id, first.id]);
    assert!(
        store
            .list_sessions_since(Timestamp(201))
            .unwrap()
            .is_empty()
    );
}

#[test]
fn a_session_records_its_cost() {
    let store = store();
    let p = project("/repos/cost");
    store.insert_project(&p).unwrap();
    let w = workspace(p.id, "cost");
    store.insert_workspace(&w).unwrap();
    let s = Session::new(w.id, AgentKind::Claude);
    store.insert_session(&s).unwrap();
    assert_eq!(store.get_session(s.id).unwrap().cost, None);
    let cost = Cost {
        amount: 0.27,
        currency: "USD".into(),
    };
    store.record_session_cost(s.id, &cost).unwrap();
    store
        .record_session_cost(
            s.id,
            &Cost {
                amount: 0.31,
                ..cost.clone()
            },
        )
        .unwrap();
    let saved = store.get_session(s.id).unwrap();
    assert_eq!(
        saved.cost.map(|c| (c.amount, c.currency)),
        Some((0.31, "USD".to_owned()))
    );
    assert!(store.record_session_cost(SessionId::new(), &cost).is_err());
}
