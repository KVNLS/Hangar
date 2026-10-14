//! The composition root for ports: the one place that picks the adapter behind each port.

use std::ffi::OsString;
use std::path::Path;
use std::sync::Arc;

use anyhow::Context as _;

use hangar_core::device::Platform;
use hangar_core::ports::{
    Changes, CodeHost, HostMonitor, ReviewOps, Store, ToolProbe, VersionControl,
};
use hangar_devices::catalog::SystemHost;
use hangar_store_sqlite::Store as SqliteStore;
use hangar_worktree::{Gh, GitCli};

use crate::setup_probe::SystemProbe;

pub fn version_control() -> &'static dyn VersionControl {
    &GitCli
}

/// The simulators (iOS) or emulators (Android) behind [`hangar_core::ports::DeviceCatalog`].
pub fn device_catalog(platform: Platform) -> &'static DeviceCatalog {
    hangar_devices::catalog::for_platform(platform)
}

/// The catalog port with the devices crate's open device.
pub type DeviceCatalog = hangar_devices::catalog::Catalog;

/// This Mac's memory, cores and memory pressure.
pub fn host_monitor() -> &'static dyn HostMonitor {
    &SystemHost
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

/// This Mac's tools, looked up in `path` (the login shell's `PATH`; `path_error` says why it could
/// not be read, if so); `gh` is checked through `code_host`.
pub fn tool_probe(
    path: String,
    path_error: Option<String>,
    gh: Arc<dyn CodeHost>,
) -> Box<dyn ToolProbe> {
    Box::new(SystemProbe {
        path,
        path_error,
        gh,
    })
}

/// Opens (creating and migrating) the SQLite database at `path`.
pub fn open_store(path: &Path) -> anyhow::Result<Arc<dyn Store>> {
    let store = SqliteStore::open(path)
        .with_context(|| format!("failed to open database {}", path.display()))?;
    Ok(Arc::new(store))
}
