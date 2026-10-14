//! The composition root for ports: the one place that picks the adapter behind each port.

use std::sync::Arc;

use hangar_core::ports::{Changes, VersionControl};
use hangar_worktree::GitCli;

pub fn version_control() -> &'static dyn VersionControl {
    &GitCli
}

pub fn changes() -> Arc<dyn Changes> {
    Arc::new(GitCli)
}
