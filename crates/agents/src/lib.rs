//! Agent adapters: interactive CLIs in a PTY, and structured agents over ACP.

pub mod acp;
mod adapter;
pub mod claude_transcript;
mod error;
pub mod ollama;
mod path;

pub use adapter::{AgentAdapter, Launch, PtyAdapter, SpawnContext, UpdateHook, presets};
pub use error::{Error, Result};
pub use path::find_program;
