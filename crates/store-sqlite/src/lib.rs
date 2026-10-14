//! SQLite persistence for projects, workspaces, sessions and app settings.
//!
//! [`Store`] is synchronous: every call takes a lock and does disk I/O. Never call it from the UI thread;
//! run it on a background thread or executor and share it via `Arc<Store>`.

use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard, PoisonError};

use rusqlite::Connection;

mod migrations;
mod port;
mod projects;
mod rows;
mod sessions;
mod settings;
mod usage;
mod workspaces;

pub use migrations::SCHEMA_VERSION;
use migrations::{migrate, user_version};

#[cfg(test)]
mod tests;

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
