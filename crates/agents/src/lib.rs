//! Agent adapters: PTY now, ACP later.

mod adapter;
mod error;
mod path;

pub use adapter::{AgentAdapter, AgentHandle, PtyAdapter, SpawnContext, UpdateHook, presets};
pub use error::{Error, Result};
pub use hangar_terminal::GridSize;
pub use path::find_program;
