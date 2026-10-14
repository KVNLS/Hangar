use rusqlite::Connection;

use crate::{Result, StoreError};

/// Ordered schema migrations; entry `i` upgrades `user_version` from `i` to `i + 1`. Append only.
pub(super) const MIGRATIONS: &[&str] = &[
    r#"
CREATE TABLE projects (
    id           TEXT PRIMARY KEY NOT NULL,
    name         TEXT NOT NULL,
    repo_path    TEXT NOT NULL UNIQUE,
    default_base TEXT NOT NULL,
    created_at   INTEGER NOT NULL
);

CREATE TABLE workspaces (
    id            TEXT PRIMARY KEY NOT NULL,
    project_id    TEXT NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
    name          TEXT NOT NULL,
    slug          TEXT NOT NULL,
    branch        TEXT NOT NULL,
    base          TEXT NOT NULL,
    worktree_path TEXT NOT NULL,
    status        TEXT NOT NULL,
    created_at    INTEGER NOT NULL,
    UNIQUE (project_id, slug)
);

CREATE TABLE sessions (
    id           TEXT PRIMARY KEY NOT NULL,
    workspace_id TEXT NOT NULL REFERENCES workspaces(id) ON DELETE CASCADE,
    agent        TEXT NOT NULL,
    command      TEXT NOT NULL,
    args         TEXT NOT NULL,
    status       TEXT NOT NULL,
    created_at   INTEGER NOT NULL,
    ended_at     INTEGER
);

CREATE INDEX sessions_workspace_id ON sessions(workspace_id);
"#,
    r#"
CREATE TABLE settings (
    key   TEXT PRIMARY KEY NOT NULL,
    value TEXT NOT NULL
);
"#,
    r#"
ALTER TABLE workspaces ADD COLUMN task TEXT;
"#,
    r#"
ALTER TABLE workspaces ADD COLUMN group_id TEXT;
"#,
    // v5: the workspace whose agent created this one (orchestration).
    r#"
ALTER TABLE workspaces ADD COLUMN parent_id TEXT;
"#,
    // v6: session history (the first prompt and the workspace diff when the session ended).
    r#"
ALTER TABLE sessions ADD COLUMN prompt TEXT;
ALTER TABLE sessions ADD COLUMN files INTEGER;
ALTER TABLE sessions ADD COLUMN insertions INTEGER;
ALTER TABLE sessions ADD COLUMN deletions INTEGER;
"#,
    // v7: what a session cost (its agent's running total).
    r#"
ALTER TABLE sessions ADD COLUMN cost_amount REAL;
ALTER TABLE sessions ADD COLUMN cost_currency TEXT;
"#,
    // v8: the usage ledger (what each model spent, per turn). Sessions that already had a cost
    // become one row each, model unknown.
    r#"
CREATE TABLE usage (
    session_id    TEXT NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
    at            INTEGER NOT NULL,
    model         TEXT,
    local         INTEGER NOT NULL,
    cost_amount   REAL,
    cost_currency TEXT,
    input         INTEGER,
    output        INTEGER,
    cache_read    INTEGER,
    cache_write   INTEGER
);

CREATE INDEX usage_at ON usage(at);

INSERT INTO usage (session_id, at, model, local, cost_amount, cost_currency)
SELECT id, COALESCE(ended_at, created_at), NULL, 0, cost_amount, cost_currency
FROM sessions WHERE cost_amount IS NOT NULL AND cost_currency IS NOT NULL;
"#,
    // v9: the ledger outlives the sessions it books (deleting a workspace or a project kept
    // erasing what was spent), so `usage.session_id` loses its cascading foreign key.
    r#"
CREATE TABLE usage_v9 (
    session_id    TEXT NOT NULL,
    at            INTEGER NOT NULL,
    model         TEXT,
    local         INTEGER NOT NULL,
    cost_amount   REAL,
    cost_currency TEXT,
    input         INTEGER,
    output        INTEGER,
    cache_read    INTEGER,
    cache_write   INTEGER
);

INSERT INTO usage_v9 SELECT session_id, at, model, local, cost_amount, cost_currency, input,
    output, cache_read, cache_write FROM usage ORDER BY rowid;
DROP TABLE usage;
ALTER TABLE usage_v9 RENAME TO usage;
CREATE INDEX usage_at ON usage(at);
"#,
];

/// Latest schema version this build understands.
pub const SCHEMA_VERSION: u32 = MIGRATIONS.len() as u32;

pub(super) fn migrate(conn: &mut Connection) -> Result<()> {
    let current = user_version(conn)?;
    if current > SCHEMA_VERSION {
        return Err(StoreError::UnsupportedSchema {
            found: current,
            supported: SCHEMA_VERSION,
        });
    }
    for (index, sql) in MIGRATIONS.iter().enumerate().skip(current as usize) {
        let target = index as u32 + 1;
        let tx = conn.transaction()?;
        tx.execute_batch(sql)?;
        tx.pragma_update(None, "user_version", target)?;
        tx.commit()?;
        tracing::info!(version = target, "applied database migration");
    }
    Ok(())
}

pub(super) fn user_version(conn: &Connection) -> rusqlite::Result<u32> {
    conn.query_row("PRAGMA user_version", [], |row| row.get(0))
}
