use std::path::PathBuf;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("{agent}: `{program}` was not found on PATH")]
    NotInstalled { agent: String, program: String },
    #[error("working directory {} does not exist or is not a directory", .0.display())]
    InvalidCwd(PathBuf),
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error("agent protocol error: {0}")]
    Protocol(String),
    #[error("{method} failed: {message} ({code})")]
    Rpc {
        method: String,
        code: i64,
        message: String,
    },
    #[error("{method}: the agent did not answer in time")]
    Timeout { method: String },
    #[error("{method}: the agent closed the connection")]
    Disconnected { method: String },
    #[error("{agent} is not signed in ({message}). {hint}")]
    NotSignedIn {
        agent: String,
        message: String,
        hint: String,
    },
    #[error("{agent} exited: {stderr_tail}")]
    AgentExited { agent: String, stderr_tail: String },
    #[error("not supported: {0}")]
    Unsupported(&'static str),
}

pub type Result<T, E = Error> = std::result::Result<T, E>;
