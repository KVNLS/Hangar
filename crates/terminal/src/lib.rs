//! Headless terminal: PTY sessions (portable-pty), emulation (alacritty_terminal), plain-data
//! grid snapshots and keyboard encoding. No UI dependencies.

mod error;
mod keys;
mod palette;
mod pty;
mod session;
mod snapshot;
mod terminal;

use std::sync::{Mutex, MutexGuard, PoisonError};

pub use error::{Result, TerminalError};
pub use keys::{Key, KeyMode, Modifiers, encode_key, encode_paste};
pub use palette::{Palette, Rgb};
pub use pty::{ExitInfo, PtyEvent, PtySession, PtyWriter, SpawnOptions};
pub use session::{SessionStats, TerminalSession};
pub use snapshot::{CellFlags, CursorShape, CursorState, Snapshot, SnapshotCell, TermModes};
pub use terminal::{GridSize, Terminal, TerminalConfig};

/// Locks a mutex, recovering the data if a panicking thread poisoned it: terminal state stays
/// usable and a crash in one consumer must not cascade into every other.
pub(crate) fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}
