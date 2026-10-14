//! SQLite persistence for projects, workspaces, sessions and app settings.
//!
//! [`Store`] is synchronous: every call takes a lock and does disk I/O. Never call it from the UI thread;
//! run it on a background thread or executor and share it via `Arc<Store>`.

use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard, PoisonError};

use rusqlite::{Connection, ErrorCode, OptionalExtension, Row, ffi, params};
use uuid::Uuid;

use crate::Setting;
use crate::model::{
    AgentKind, Project, ProjectId, Session, SessionId, SessionStatus, Timestamp, Workspace,
    WorkspaceId, WorkspaceStatus,
};

#[cfg(test)]
mod tests;

/// Ordered schema migrations; entry `i` upgrades `user_version` from `i` to `i + 1`. Append only.
const MIGRATIONS: &[&str] = &[
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
];

/// Latest schema version this build understands.
pub const SCHEMA_VERSION: u32 = MIGRATIONS.len() as u32;

#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    #[error("{entity} {id} not found")]
    NotFound { entity: &'static str, id: String },
    #[error("conflict: {0}")]
    Conflict(String),
    #[error("path is not valid UTF-8: {}", .0.display())]
    NonUtf8Path(PathBuf),
    #[error("database schema version {found} is newer than supported version {supported}")]
    UnsupportedSchema { found: u32, supported: u32 },
    #[error("failed to create database directory {}: {source}", .path.display())]
    CreateDir {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error(transparent)]
    Sqlite(#[from] rusqlite::Error),
    #[error(transparent)]
    Serde(#[from] serde_json::Error),
}

pub type Result<T, E = StoreError> = std::result::Result<T, E>;

/// Thread-safe (`Send + Sync`) handle to the Hangar database.
pub struct Store {
    conn: Mutex<Connection>,
}

impl Store {
    /// Opens (creating if needed) the database at `path` and applies pending migrations.
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref();
        if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
            std::fs::create_dir_all(parent).map_err(|source| StoreError::CreateDir {
                path: parent.to_path_buf(),
                source,
            })?;
        }
        Self::init(Connection::open(path)?)
    }

    pub fn open_in_memory() -> Result<Self> {
        Self::init(Connection::open_in_memory()?)
    }

    fn init(mut conn: Connection) -> Result<Self> {
        let mode: String = conn.query_row("PRAGMA journal_mode = WAL", [], |row| row.get(0))?;
        tracing::debug!(journal_mode = %mode, "opened database");
        conn.pragma_update(None, "foreign_keys", true)?;
        conn.busy_timeout(std::time::Duration::from_secs(5))?;
        migrate(&mut conn)?;
        Ok(Self {
            conn: Mutex::new(conn),
        })
    }

    /// Current `PRAGMA user_version` of the database.
    pub fn schema_version(&self) -> Result<u32> {
        Ok(user_version(&self.conn())?)
    }

    fn conn(&self) -> MutexGuard<'_, Connection> {
        // A panic while holding the lock cannot leave SQLite half-written: statements are atomic.
        self.conn.lock().unwrap_or_else(PoisonError::into_inner)
    }

    pub fn insert_project(&self, project: &Project) -> Result<()> {
        let repo_path = path_str(&project.repo_path)?;
        self.conn()
            .execute(
                "INSERT INTO projects (id, name, repo_path, default_base, created_at)
                 VALUES (?1, ?2, ?3, ?4, ?5)",
                params![
                    project.id.0.to_string(),
                    project.name,
                    repo_path,
                    project.default_base,
                    project.created_at.0,
                ],
            )
            .map_err(|e| {
                map_write_error(
                    e,
                    || format!("a project already exists at {repo_path}"),
                    None,
                )
            })?;
        Ok(())
    }

    pub fn list_projects(&self) -> Result<Vec<Project>> {
        let conn = self.conn();
        let mut stmt = conn.prepare(
            "SELECT id, name, repo_path, default_base, created_at
             FROM projects ORDER BY created_at, rowid",
        )?;
        let rows = stmt.query_map([], project_from_row)?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    pub fn get_project(&self, id: ProjectId) -> Result<Project> {
        self.conn()
            .query_row(
                "SELECT id, name, repo_path, default_base, created_at FROM projects WHERE id = ?1",
                [id.0.to_string()],
                project_from_row,
            )
            .optional()?
            .ok_or_else(|| not_found("project", id))
    }

    /// Deletes the project and, by cascade, its workspaces and sessions.
    pub fn delete_project(&self, id: ProjectId) -> Result<()> {
        let changed = self
            .conn()
            .execute("DELETE FROM projects WHERE id = ?1", [id.0.to_string()])?;
        ensure_changed(changed, "project", id)
    }

    pub fn insert_workspace(&self, workspace: &Workspace) -> Result<()> {
        let worktree_path = path_str(&workspace.worktree_path)?;
        self.conn()
            .execute(
                "INSERT INTO workspaces
                 (id, project_id, name, slug, branch, base, worktree_path, status, created_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
                params![
                    workspace.id.0.to_string(),
                    workspace.project_id.0.to_string(),
                    workspace.name,
                    workspace.slug,
                    workspace.branch,
                    workspace.base,
                    worktree_path,
                    workspace.status.as_str(),
                    workspace.created_at.0,
                ],
            )
            .map_err(|e| {
                map_write_error(
                    e,
                    || {
                        format!(
                            "workspace slug `{}` is already used in this project",
                            workspace.slug
                        )
                    },
                    Some(not_found("project", workspace.project_id)),
                )
            })?;
        Ok(())
    }

    pub fn list_workspaces(&self, project_id: ProjectId) -> Result<Vec<Workspace>> {
        let conn = self.conn();
        let mut stmt = conn.prepare(
            "SELECT id, project_id, name, slug, branch, base, worktree_path, status, created_at
             FROM workspaces WHERE project_id = ?1 ORDER BY created_at, rowid",
        )?;
        let rows = stmt.query_map([project_id.0.to_string()], workspace_from_row)?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    pub fn get_workspace(&self, id: WorkspaceId) -> Result<Workspace> {
        self.conn()
            .query_row(
                "SELECT id, project_id, name, slug, branch, base, worktree_path, status, created_at
                 FROM workspaces WHERE id = ?1",
                [id.0.to_string()],
                workspace_from_row,
            )
            .optional()?
            .ok_or_else(|| not_found("workspace", id))
    }

    pub fn update_workspace_status(&self, id: WorkspaceId, status: WorkspaceStatus) -> Result<()> {
        let changed = self.conn().execute(
            "UPDATE workspaces SET status = ?2 WHERE id = ?1",
            params![id.0.to_string(), status.as_str()],
        )?;
        ensure_changed(changed, "workspace", id)
    }

    /// Deletes the workspace and, by cascade, its sessions.
    pub fn delete_workspace(&self, id: WorkspaceId) -> Result<()> {
        let changed = self
            .conn()
            .execute("DELETE FROM workspaces WHERE id = ?1", [id.0.to_string()])?;
        ensure_changed(changed, "workspace", id)
    }

    pub fn insert_session(&self, session: &Session) -> Result<()> {
        let agent = serde_json::to_string(&session.agent)?;
        let args = serde_json::to_string(&session.args)?;
        let status = serde_json::to_string(&session.status)?;
        self.conn()
            .execute(
                "INSERT INTO sessions
                 (id, workspace_id, agent, command, args, status, created_at, ended_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
                params![
                    session.id.0.to_string(),
                    session.workspace_id.0.to_string(),
                    agent,
                    session.command,
                    args,
                    status,
                    session.created_at.0,
                    session.ended_at.map(|t| t.0),
                ],
            )
            .map_err(|e| {
                map_write_error(
                    e,
                    || format!("session {} already exists", session.id),
                    Some(not_found("workspace", session.workspace_id)),
                )
            })?;
        Ok(())
    }

    pub fn list_sessions(&self, workspace_id: WorkspaceId) -> Result<Vec<Session>> {
        let conn = self.conn();
        let mut stmt = conn.prepare(
            "SELECT id, workspace_id, agent, command, args, status, created_at, ended_at
             FROM sessions WHERE workspace_id = ?1 ORDER BY created_at, rowid",
        )?;
        let rows = stmt.query_map([workspace_id.0.to_string()], session_from_row)?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    pub fn get_session(&self, id: SessionId) -> Result<Session> {
        self.conn()
            .query_row(
                "SELECT id, workspace_id, agent, command, args, status, created_at, ended_at
                 FROM sessions WHERE id = ?1",
                [id.0.to_string()],
                session_from_row,
            )
            .optional()?
            .ok_or_else(|| not_found("session", id))
    }

    /// Updates the status; a terminal status (`Exited`/`Failed`) also stamps `ended_at` once.
    /// Returns the updated session.
    pub fn update_session_status(&self, id: SessionId, status: &SessionStatus) -> Result<Session> {
        let encoded = serde_json::to_string(status)?;
        let ended_at = status.is_terminal().then(|| Timestamp::now().0);
        let changed = self.conn().execute(
            "UPDATE sessions SET status = ?2, ended_at = COALESCE(ended_at, ?3) WHERE id = ?1",
            params![id.0.to_string(), encoded, ended_at],
        )?;
        ensure_changed(changed, "session", id)?;
        self.get_session(id)
    }

    /// Reads a JSON-encoded setting; `None` when the key was never set.
    pub fn get_setting<T: serde::de::DeserializeOwned>(
        &self,
        setting: &Setting<T>,
    ) -> Result<Option<T>> {
        let value: Option<String> = self
            .conn()
            .query_row(
                "SELECT value FROM settings WHERE key = ?1",
                [setting.key()],
                |row| row.get(0),
            )
            .optional()?;
        value
            .map(|v| serde_json::from_str(&v))
            .transpose()
            .map_err(StoreError::from)
    }

    /// Stores `value` as JSON under `setting`, replacing any previous value.
    pub fn set_setting<T: serde::Serialize>(&self, setting: &Setting<T>, value: &T) -> Result<()> {
        let encoded = serde_json::to_string(value)?;
        self.conn().execute(
            "INSERT INTO settings (key, value) VALUES (?1, ?2)
             ON CONFLICT(key) DO UPDATE SET value = excluded.value",
            params![setting.key(), encoded],
        )?;
        Ok(())
    }

    /// Removes `setting`; removing a missing one is not an error.
    pub fn delete_setting<T>(&self, setting: &Setting<T>) -> Result<()> {
        self.conn()
            .execute("DELETE FROM settings WHERE key = ?1", [setting.key()])?;
        Ok(())
    }
}

/// Default database location: `~/Library/Application Support/Hangar/hangar.db` on macOS,
/// `$XDG_DATA_HOME/hangar/hangar.db` (or `~/.local/share/hangar/hangar.db`) elsewhere.
/// `None` when no home directory can be determined.
pub fn default_db_path() -> Option<PathBuf> {
    db_path_for(
        cfg!(target_os = "macos"),
        std::env::var_os("HOME").map(PathBuf::from),
        std::env::var_os("XDG_DATA_HOME").map(PathBuf::from),
    )
}

fn db_path_for(
    macos: bool,
    home: Option<PathBuf>,
    xdg_data_home: Option<PathBuf>,
) -> Option<PathBuf> {
    let home = home.filter(|p| p.is_absolute());
    if macos {
        return Some(home?.join("Library/Application Support/Hangar/hangar.db"));
    }
    let data_home = match xdg_data_home.filter(|p| p.is_absolute()) {
        Some(dir) => dir,
        None => home?.join(".local/share"),
    };
    Some(data_home.join("hangar/hangar.db"))
}

fn migrate(conn: &mut Connection) -> Result<()> {
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

fn user_version(conn: &Connection) -> rusqlite::Result<u32> {
    conn.query_row("PRAGMA user_version", [], |row| row.get(0))
}

fn path_str(path: &Path) -> Result<&str> {
    path.to_str()
        .ok_or_else(|| StoreError::NonUtf8Path(path.to_path_buf()))
}

fn not_found(entity: &'static str, id: impl ToString) -> StoreError {
    StoreError::NotFound {
        entity,
        id: id.to_string(),
    }
}

fn ensure_changed(changed: usize, entity: &'static str, id: impl ToString) -> Result<()> {
    if changed == 0 {
        Err(not_found(entity, id))
    } else {
        Ok(())
    }
}

/// Maps unique/primary-key violations to `Conflict` and foreign-key violations to `missing_parent`.
fn map_write_error(
    error: rusqlite::Error,
    conflict: impl FnOnce() -> String,
    missing_parent: Option<StoreError>,
) -> StoreError {
    if let rusqlite::Error::SqliteFailure(failure, _) = &error
        && failure.code == ErrorCode::ConstraintViolation
    {
        match failure.extended_code {
            ffi::SQLITE_CONSTRAINT_UNIQUE | ffi::SQLITE_CONSTRAINT_PRIMARYKEY => {
                return StoreError::Conflict(conflict());
            }
            ffi::SQLITE_CONSTRAINT_FOREIGNKEY => {
                if let Some(parent) = missing_parent {
                    return parent;
                }
            }
            _ => {}
        }
    }
    StoreError::Sqlite(error)
}

fn conversion_error(
    index: usize,
    error: impl std::error::Error + Send + Sync + 'static,
) -> rusqlite::Error {
    rusqlite::Error::FromSqlConversionFailure(index, rusqlite::types::Type::Text, Box::new(error))
}

fn uuid_at(row: &Row<'_>, index: usize) -> rusqlite::Result<Uuid> {
    let text: String = row.get(index)?;
    Uuid::parse_str(&text).map_err(|e| conversion_error(index, e))
}

fn json_at<T: serde::de::DeserializeOwned>(row: &Row<'_>, index: usize) -> rusqlite::Result<T> {
    let text: String = row.get(index)?;
    serde_json::from_str(&text).map_err(|e| conversion_error(index, e))
}

#[derive(Debug, thiserror::Error)]
#[error("unknown workspace status `{0}`")]
struct UnknownStatus(String);

fn project_from_row(row: &Row<'_>) -> rusqlite::Result<Project> {
    Ok(Project {
        id: ProjectId(uuid_at(row, 0)?),
        name: row.get(1)?,
        repo_path: PathBuf::from(row.get::<_, String>(2)?),
        default_base: row.get(3)?,
        created_at: Timestamp(row.get(4)?),
    })
}

fn workspace_from_row(row: &Row<'_>) -> rusqlite::Result<Workspace> {
    let status: String = row.get(7)?;
    let status = WorkspaceStatus::parse(&status)
        .ok_or_else(|| conversion_error(7, UnknownStatus(status)))?;
    Ok(Workspace {
        id: WorkspaceId(uuid_at(row, 0)?),
        project_id: ProjectId(uuid_at(row, 1)?),
        name: row.get(2)?,
        slug: row.get(3)?,
        branch: row.get(4)?,
        base: row.get(5)?,
        worktree_path: PathBuf::from(row.get::<_, String>(6)?),
        status,
        created_at: Timestamp(row.get(8)?),
    })
}

fn session_from_row(row: &Row<'_>) -> rusqlite::Result<Session> {
    Ok(Session {
        id: SessionId(uuid_at(row, 0)?),
        workspace_id: WorkspaceId(uuid_at(row, 1)?),
        agent: json_at::<AgentKind>(row, 2)?,
        command: row.get(3)?,
        args: json_at(row, 4)?,
        status: json_at(row, 5)?,
        created_at: Timestamp(row.get(6)?),
        ended_at: row.get::<_, Option<i64>>(7)?.map(Timestamp),
    })
}
