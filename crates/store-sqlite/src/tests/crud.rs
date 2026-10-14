use super::*;

#[test]
fn project_crud_round_trip() {
    let store = store();
    let a = project("/repos/a");
    let b = project("/repos/b");
    store.insert_project(&a).unwrap();
    store.insert_project(&b).unwrap();

    assert_eq!(store.get_project(a.id).unwrap(), a);
    assert_eq!(store.list_projects().unwrap(), vec![a.clone(), b.clone()]);

    store.delete_project(a.id).unwrap();
    assert_eq!(store.list_projects().unwrap(), vec![b]);
    assert!(matches!(
        store.get_project(a.id),
        Err(StoreError::NotFound {
            entity: "project",
            ..
        })
    ));
    assert!(matches!(
        store.delete_project(a.id),
        Err(StoreError::NotFound { .. })
    ));
}

#[test]
fn duplicate_repo_path_is_a_conflict() {
    let store = store();
    store.insert_project(&project("/repos/a")).unwrap();
    assert!(matches!(
        store.insert_project(&project("/repos/a")),
        Err(StoreError::Conflict(_))
    ));
}

#[test]
fn workspace_crud_round_trip() {
    let store = store();
    let p = project("/repos/a");
    store.insert_project(&p).unwrap();
    let w1 = workspace(p.id, "Fix login");
    let w2 = workspace(p.id, "Add OAuth");
    store.insert_workspace(&w1).unwrap();
    store.insert_workspace(&w2).unwrap();

    assert_eq!(store.get_workspace(w1.id).unwrap(), w1);
    assert_eq!(
        store.list_workspaces(p.id).unwrap(),
        vec![w1.clone(), w2.clone()]
    );
    assert!(store.list_workspaces(ProjectId::new()).unwrap().is_empty());

    store
        .update_workspace_status(w1.id, WorkspaceStatus::Archived)
        .unwrap();
    assert_eq!(
        store.get_workspace(w1.id).unwrap().status,
        WorkspaceStatus::Archived
    );

    store.delete_workspace(w1.id).unwrap();
    assert_eq!(store.list_workspaces(p.id).unwrap(), vec![w2]);
    assert!(matches!(
        store.update_workspace_status(w1.id, WorkspaceStatus::Active),
        Err(StoreError::NotFound {
            entity: "workspace",
            ..
        })
    ));
}

#[test]
fn duplicate_slug_conflicts_only_within_a_project() {
    let store = store();
    let a = project("/repos/a");
    let b = project("/repos/b");
    store.insert_project(&a).unwrap();
    store.insert_project(&b).unwrap();
    store.insert_workspace(&workspace(a.id, "task")).unwrap();
    store.insert_workspace(&workspace(b.id, "task")).unwrap();
    assert!(matches!(
        store.insert_workspace(&workspace(a.id, "task")),
        Err(StoreError::Conflict(_))
    ));
}

#[test]
fn workspace_for_missing_project_is_not_found() {
    let store = store();
    assert!(matches!(
        store.insert_workspace(&workspace(ProjectId::new(), "orphan")),
        Err(StoreError::NotFound {
            entity: "project",
            ..
        })
    ));
}

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
fn deleting_a_project_cascades() {
    let store = store();
    let p = project("/repos/a");
    store.insert_project(&p).unwrap();
    let w = workspace(p.id, "task");
    store.insert_workspace(&w).unwrap();
    let s = Session::new(w.id, AgentKind::Codex);
    store.insert_session(&s).unwrap();

    store.delete_project(p.id).unwrap();
    assert!(matches!(
        store.get_workspace(w.id),
        Err(StoreError::NotFound { .. })
    ));
    assert!(matches!(
        store.get_session(s.id),
        Err(StoreError::NotFound { .. })
    ));
}

#[test]
fn deleting_a_workspace_cascades_to_sessions() {
    let store = store();
    let p = project("/repos/a");
    store.insert_project(&p).unwrap();
    let w = workspace(p.id, "task");
    store.insert_workspace(&w).unwrap();
    store
        .insert_session(&Session::new(w.id, AgentKind::Gemini))
        .unwrap();

    store.delete_workspace(w.id).unwrap();
    assert!(store.list_sessions(w.id).unwrap().is_empty());
}

#[test]
fn store_is_shareable_across_threads() {
    let store = Arc::new(store());
    let handles: Vec<_> = (0..4)
        .map(|i| {
            let store = Arc::clone(&store);
            std::thread::spawn(move || {
                store
                    .insert_project(&project(&format!("/repos/{i}")))
                    .unwrap();
            })
        })
        .collect();
    for handle in handles {
        handle.join().unwrap();
    }
    assert_eq!(store.list_projects().unwrap().len(), 4);
}

#[cfg(unix)]
#[test]
fn non_utf8_paths_are_rejected() {
    use std::ffi::OsStr;
    use std::os::unix::ffi::OsStrExt;

    let store = store();
    let p = Project::new("Bad", OsStr::from_bytes(b"/repos/\xff"), "main");
    assert!(matches!(
        store.insert_project(&p),
        Err(StoreError::NonUtf8Path(_))
    ));
}

#[test]
fn a_workspace_task_and_group_round_trip() {
    let store = store();
    let p = project("/repos/task");
    store.insert_project(&p).unwrap();
    let ws = Workspace {
        task: Some("Fix the login screen\nand its tests".to_owned()),
        group: Some(WorkspaceGroupId::new()),
        ..workspace(p.id, "Login")
    };
    store.insert_workspace(&ws).unwrap();
    assert_eq!(store.get_workspace(ws.id).unwrap(), ws);
    assert_eq!(store.list_workspaces(p.id).unwrap(), vec![ws]);
}

#[test]
fn a_workspace_parent_round_trips() {
    let store = store();
    let p = project("/repos/parent");
    store.insert_project(&p).unwrap();
    let lead = workspace(p.id, "Lead");
    let child = Workspace {
        parent: Some(lead.id),
        ..workspace(p.id, "Child")
    };
    store.insert_workspace(&lead).unwrap();
    store.insert_workspace(&child).unwrap();
    assert_eq!(store.get_workspace(child.id).unwrap(), child);
    assert_eq!(store.list_workspaces(p.id).unwrap(), vec![lead, child]);
}

#[test]
fn unreadable_rows_are_skipped_not_fatal() {
    let store = store();
    let p = project("/repos/demo");
    store.insert_project(&p).unwrap();
    let good = workspace(p.id, "Good");
    let bad = workspace(p.id, "Bad");
    store.insert_workspace(&good).unwrap();
    store.insert_workspace(&bad).unwrap();
    let ok = Session::new(good.id, AgentKind::Shell);
    let odd = Session::new(good.id, AgentKind::Shell);
    store.insert_session(&ok).unwrap();
    store.insert_session(&odd).unwrap();
    {
        let conn = store.conn();
        conn.execute(
            "UPDATE workspaces SET status = 'paused' WHERE id = ?1",
            [bad.id.0.to_string()],
        )
        .unwrap();
        conn.execute(
            "UPDATE sessions SET agent = 'from-a-newer-hangar' WHERE id = ?1",
            [odd.id.0.to_string()],
        )
        .unwrap();
    }
    let workspaces = store.list_workspaces(p.id).unwrap();
    assert_eq!(
        workspaces.iter().map(|w| w.id).collect::<Vec<_>>(),
        [good.id]
    );
    let sessions = store.list_sessions(good.id).unwrap();
    assert_eq!(sessions.iter().map(|s| s.id).collect::<Vec<_>>(), [ok.id]);
}
