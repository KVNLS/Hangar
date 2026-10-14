use std::io;

/// Errors raised by PTY sessions and terminal emulation.
#[derive(Debug, thiserror::Error)]
pub enum TerminalError {
    #[error("failed to open a pseudo-terminal: {0}")]
    OpenPty(String),
    #[error("failed to spawn `{program}`: {reason}")]
    Spawn { program: String, reason: String },
    #[error("failed to resize the pseudo-terminal: {0}")]
    Resize(String),
    #[error("failed to set up PTY I/O: {0}")]
    PtyIo(String),
    #[error("failed to start the {0} thread: {1}")]
    Thread(&'static str, io::Error),
    #[error("PTY write failed: {0}")]
    Write(#[source] io::Error),
    #[error("waiting for the child process failed: {0}")]
    Wait(String),
    #[error("failed to kill the child process: {0}")]
    Kill(#[source] io::Error),
    #[error("grid size must be at least 1x1 (got {cols}x{rows})")]
    InvalidSize { cols: u16, rows: u16 },
}

pub type Result<T, E = TerminalError> = std::result::Result<T, E>;
