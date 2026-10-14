use std::path::PathBuf;
use std::sync::Arc;

use super::*;
use hangar_core::slugify;

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
        task: None,
        group: None,
        parent: None,
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

#[test]
fn v1_database_file_is_migrated_to_settings() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("hangar.db");
    let p = project("/repos/v1");
    {
        // A database as written by a v1 build: first migration only.
        let conn = Connection::open(&path).unwrap();
        conn.execute_batch(MIGRATIONS[0]).unwrap();
        conn.pragma_update(None, "user_version", 1).unwrap();
        conn.execute(
            "INSERT INTO projects (id, name, repo_path, default_base, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            params![
                p.id.0.to_string(),
                p.name,
                "/repos/v1",
                p.default_base,
                p.created_at.0
            ],
        )
        .unwrap();
    }
    let store = Store::open(&path).unwrap();
    assert_eq!(store.schema_version().unwrap(), SCHEMA_VERSION);
    assert_eq!(store.list_projects().unwrap(), vec![p]);
    const AGENT: Setting<String> = Setting::new("agent");
    assert_eq!(store.get_setting(&AGENT).unwrap(), None);
    store.set_setting(&AGENT, &"codex".to_owned()).unwrap();
    drop(store);
    let store = Store::open(&path).unwrap();
    assert_eq!(store.get_setting(&AGENT).unwrap().as_deref(), Some("codex"));
}

#[test]
fn settings_round_trip_and_overwrite() {
    #[derive(Debug, PartialEq, serde::Serialize, serde::Deserialize)]
    struct Layout {
        panes: Vec<Option<WorkspaceId>>,
        split: Option<f32>,
    }
    let store = store();
    let layout = Layout {
        panes: vec![Some(WorkspaceId::new()), None],
        split: Some(0.4),
    };
    const LAYOUT: Setting<Layout> = Setting::new("layout");
    store.set_setting(&LAYOUT, &layout).unwrap();
    assert_eq!(store.get_setting(&LAYOUT).unwrap(), Some(layout));

    let empty = Layout {
        panes: vec![],
        split: None,
    };
    store.set_setting(&LAYOUT, &empty).unwrap();
    assert_eq!(store.get_setting(&LAYOUT).unwrap(), Some(empty));

    // A value of the wrong shape is an error, not a silent default.
    store
        .set_setting(&Setting::new("layout"), &"not a layout")
        .unwrap();
    assert!(matches!(
        store.get_setting(&LAYOUT),
        Err(StoreError::Serde(_))
    ));
}

#[test]
fn delete_setting_removes_the_key() {
    let store = Store::open_in_memory().unwrap();
    let run = Setting::<u16>::scoped("run", "x");
    store.set_setting(&run, &8100).unwrap();
    store.delete_setting(&run).unwrap();
    assert_eq!(store.get_setting(&run).unwrap(), None);
    store
        .delete_setting(&Setting::<u16>::new("missing"))
        .unwrap();
}

#[test]
fn v2_workspaces_migrate_without_a_task() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("hangar.db");
    let p = project("/repos/v2");
    let ws = workspace(p.id, "Old");
    {
        // A database as written by a v2 build: no `task` column yet.
        let conn = Connection::open(&path).unwrap();
        conn.execute_batch(MIGRATIONS[0]).unwrap();
        conn.execute_batch(MIGRATIONS[1]).unwrap();
        conn.pragma_update(None, "user_version", 2).unwrap();
        conn.execute(
            "INSERT INTO projects (id, name, repo_path, default_base, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            params![
                p.id.0.to_string(),
                p.name,
                "/repos/v2",
                p.default_base,
                p.created_at.0
            ],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO workspaces
             (id, project_id, name, slug, branch, base, worktree_path, status, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            params![
                ws.id.0.to_string(),
                p.id.0.to_string(),
                ws.name,
                ws.slug,
                ws.branch,
                ws.base,
                "/tmp/worktrees/old",
                "active",
                ws.created_at.0
            ],
        )
        .unwrap();
    }
    let store = Store::open(&path).unwrap();
    assert_eq!(store.schema_version().unwrap(), SCHEMA_VERSION);
    assert_eq!(store.get_workspace(ws.id).unwrap(), ws);
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
fn sessions_before_history_migrate_without_prompt_or_changes() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("hangar.db");
    let p = project("/repos/history");
    let ws = workspace(p.id, "Old");
    let mut s = Session::new(ws.id, AgentKind::Claude);
    s.status = SessionStatus::Exited { code: 0 };
    s.ended_at = Some(Timestamp(5));
    {
        // A database as written by the build before session history (v5).
        let conn = Connection::open(&path).unwrap();
        for sql in &MIGRATIONS[..5] {
            conn.execute_batch(sql).unwrap();
        }
        conn.pragma_update(None, "user_version", 5).unwrap();
        conn.execute(
            "INSERT INTO projects (id, name, repo_path, default_base, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            params![p.id.0.to_string(), p.name, "/repos/history", "main", 0],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO workspaces
             (id, project_id, name, slug, branch, base, worktree_path, status, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            params![
                ws.id.0.to_string(),
                p.id.0.to_string(),
                ws.name,
                ws.slug,
                ws.branch,
                ws.base,
                "/tmp/worktrees/old",
                "active",
                ws.created_at.0
            ],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO sessions
             (id, workspace_id, agent, command, args, status, created_at, ended_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            params![
                s.id.0.to_string(),
                ws.id.0.to_string(),
                serde_json::to_string(&s.agent).unwrap(),
                s.command,
                "[]",
                serde_json::to_string(&s.status).unwrap(),
                s.created_at.0,
                5
            ],
        )
        .unwrap();
    }
    let store = Store::open(&path).unwrap();
    assert_eq!(store.schema_version().unwrap(), SCHEMA_VERSION);
    assert_eq!(store.get_session(s.id).unwrap(), s);
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
fn v4_workspaces_migrate_without_a_parent() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("hangar.db");
    let p = project("/repos/v4");
    let ws = Workspace {
        task: Some("Fix it".to_owned()),
        group: Some(WorkspaceGroupId::new()),
        ..workspace(p.id, "Old")
    };
    {
        // A database as written by a v4 build: no `parent_id` column yet.
        let conn = Connection::open(&path).unwrap();
        for sql in &MIGRATIONS[..4] {
            conn.execute_batch(sql).unwrap();
        }
        conn.pragma_update(None, "user_version", 4).unwrap();
        conn.execute(
            "INSERT INTO projects (id, name, repo_path, default_base, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            params![
                p.id.0.to_string(),
                p.name,
                "/repos/v4",
                p.default_base,
                p.created_at.0
            ],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO workspaces
             (id, project_id, name, slug, branch, base, worktree_path, status, created_at, task,
              group_id)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
            params![
                ws.id.0.to_string(),
                p.id.0.to_string(),
                ws.name,
                ws.slug,
                ws.branch,
                ws.base,
                "/tmp/worktrees/old",
                "active",
                ws.created_at.0,
                ws.task,
                ws.group.map(|g| g.0.to_string()),
            ],
        )
        .unwrap();
    }
    let store = Store::open(&path).unwrap();
    assert!(store.schema_version().unwrap() >= 5);
    assert_eq!(store.get_workspace(ws.id).unwrap(), ws);
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
    store.delete_workspace(w.id).unwrap();
    assert!(store.list_usage_since(Timestamp(0)).unwrap().is_empty());
}

#[test]
fn v7_session_costs_become_usage_rows() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("hangar.db");
    let p = project("/repos/v7");
    let w = workspace(p.id, "Old");
    let mut paid = Session::new(w.id, AgentKind::Claude);
    paid.ended_at = Some(Timestamp(5_000));
    paid.cost = Some(Cost {
        amount: 1.5,
        currency: "USD".into(),
    });
    let free = Session::new(w.id, AgentKind::Claude);
    {
        let store = Store::open(&path).unwrap();
        store.insert_project(&p).unwrap();
        store.insert_workspace(&w).unwrap();
        store.insert_session(&paid).unwrap();
        store.insert_session(&free).unwrap();
        let conn = store.conn();
        conn.execute_batch("DROP TABLE usage").unwrap();
        conn.pragma_update(None, "user_version", 7).unwrap();
    }
    let store = Store::open(&path).unwrap();
    assert_eq!(store.schema_version().unwrap(), SCHEMA_VERSION);
    assert_eq!(
        store.list_usage_since(Timestamp(0)).unwrap(),
        vec![UsageEntry {
            session_id: paid.id,
            at: Timestamp(5_000),
            model: None,
            local: false,
            cost: paid.cost,
            tokens: None,
        }]
    );
}
