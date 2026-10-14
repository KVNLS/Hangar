use std::path::PathBuf;
use std::sync::Arc;

use super::*;
use crate::slugify;

fn store() -> Store {
    Store::open_in_memory().unwrap()
}

fn project(path: &str) -> Project {
    Project::new("Demo", path, "main")
}

fn workspace(project_id: ProjectId, name: &str) -> Workspace {
    let slug = slugify(name);
    Workspace {
        id: WorkspaceId::new(),
        project_id,
        name: name.to_owned(),
        branch: format!("hangar/{slug}"),
        base: "main".to_owned(),
        worktree_path: PathBuf::from(format!("/tmp/worktrees/{slug}")),
        slug,
        status: WorkspaceStatus::Active,
        created_at: Timestamp::now(),
    }
}

#[test]
fn fresh_file_database_is_migrated_and_reopens() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("nested/hangar.db");
    let p = project("/repos/demo");
    {
        let store = Store::open(&path).unwrap();
        assert_eq!(store.schema_version().unwrap(), SCHEMA_VERSION);
        store.insert_project(&p).unwrap();
    }
    let store = Store::open(&path).unwrap();
    assert_eq!(store.schema_version().unwrap(), SCHEMA_VERSION);
    assert_eq!(store.list_projects().unwrap(), vec![p]);
    let mode: String = store
        .conn()
        .query_row("PRAGMA journal_mode", [], |r| r.get(0))
        .unwrap();
    assert_eq!(mode, "wal");
    let fk: bool = store
        .conn()
        .query_row("PRAGMA foreign_keys", [], |r| r.get(0))
        .unwrap();
    assert!(fk);
}

#[test]
fn newer_schema_is_rejected() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("hangar.db");
    Store::open(&path)
        .unwrap()
        .conn()
        .pragma_update(None, "user_version", SCHEMA_VERSION + 1)
        .unwrap();
    assert!(matches!(
        Store::open(&path),
        Err(StoreError::UnsupportedSchema { .. })
    ));
}

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
fn db_path_resolution() {
    let home = Some(PathBuf::from("/home/u"));
    assert_eq!(
        db_path_for(true, home.clone(), None),
        Some(PathBuf::from(
            "/home/u/Library/Application Support/Hangar/hangar.db"
        ))
    );
    assert_eq!(
        db_path_for(false, home.clone(), None),
        Some(PathBuf::from("/home/u/.local/share/hangar/hangar.db"))
    );
    assert_eq!(
        db_path_for(false, home.clone(), Some(PathBuf::from("/data"))),
        Some(PathBuf::from("/data/hangar/hangar.db"))
    );
    assert_eq!(
        db_path_for(false, home, Some(PathBuf::from("relative"))),
        Some(PathBuf::from("/home/u/.local/share/hangar/hangar.db"))
    );
    assert_eq!(db_path_for(true, None, None), None);
    assert!(default_db_path().is_some_and(|p| p.ends_with("hangar.db")));
}
