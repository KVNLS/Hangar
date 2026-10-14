//! File watching and the live diff model of a worktree. Headless: all work runs on background
//! threads and results arrive on tokio unbounded channels.

mod error;
mod live;
mod watcher;

pub use error::{Error, Result};
pub use live::{DiffSummary, DiffUpdate, LiveDiff, LiveDiffConfig};
pub use watcher::{ChangeBatch, Watcher, WatcherConfig};
