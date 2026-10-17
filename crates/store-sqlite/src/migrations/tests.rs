use rusqlite::{Connection, params};

use hangar_core::Setting;
use hangar_core::changes::{Cost, UsageEntry};
use hangar_core::{AgentKind, Session, SessionStatus, Timestamp, Workspace, WorkspaceGroupId};

use super::MIGRATIONS;
use crate::fixtures::{project, workspace};
use crate::{SCHEMA_VERSION, Store, StoreError};

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
