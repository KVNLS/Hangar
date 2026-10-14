//! The composition root for ports: the one place that picks the adapter behind each port.

use std::path::Path;
use std::sync::Arc;

use anyhow::Context as _;

use hangar_core::ports::{Changes, Store, VersionControl};
use hangar_store_sqlite::Store as SqliteStore;
use hangar_worktree::GitCli;

pub fn version_control() -> &'static dyn VersionControl {
    &GitCli
}

pub fn changes() -> Arc<dyn Changes> {
    Arc::new(GitCli)
}

/// Opens (creating and migrating) the SQLite database at `path`.
pub fn open_store(path: &Path) -> anyhow::Result<Arc<dyn Store>> {
    let store = SqliteStore::open(path)
        .with_context(|| format!("failed to open database {}", path.display()))?;
    Ok(Arc::new(store))
}
