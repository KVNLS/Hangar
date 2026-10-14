use std::path::PathBuf;

use hangar_terminal::TerminalError;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("{agent}: `{program}` was not found on PATH")]
    NotInstalled { agent: String, program: String },
    #[error("working directory {} does not exist or is not a directory", .0.display())]
    InvalidCwd(PathBuf),
    #[error(transparent)]
    Terminal(#[from] TerminalError),
}

pub type Result<T, E = Error> = std::result::Result<T, E>;
