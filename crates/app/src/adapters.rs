//! The composition root for ports: the one place that picks the adapter behind each port.

use std::ffi::OsString;
use std::path::Path;
use std::sync::Arc;

use anyhow::Context as _;

use hangar_core::ports::{Changes, CodeHost, ReviewOps, Store, VersionControl};
use hangar_store_sqlite::Store as SqliteStore;
use hangar_worktree::{Gh, GitCli};

pub fn version_control() -> &'static dyn VersionControl {
    &GitCli
}

pub fn changes() -> Arc<dyn Changes> {
    Arc::new(GitCli)
}

pub fn review_ops() -> &'static dyn ReviewOps {
    &GitCli
}

/// `gh` (or `program`), looked up in and run with `path_env` (a GUI app's own PATH usually
/// lacks Homebrew).
pub fn code_host(path_env: impl Into<OsString>, program: Option<OsString>) -> Arc<dyn CodeHost> {
    let mut gh = Gh::new().with_path_env(path_env);
    if let Some(program) = program {
        gh = gh.with_program(program);
    }
    Arc::new(gh)
}

/// Opens (creating and migrating) the SQLite database at `path`.
pub fn open_store(path: &Path) -> anyhow::Result<Arc<dyn Store>> {
    let store = SqliteStore::open(path)
        .with_context(|| format!("failed to open database {}", path.display()))?;
    Ok(Arc::new(store))
}
