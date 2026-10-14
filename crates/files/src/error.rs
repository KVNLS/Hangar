use std::io;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error(transparent)]
    Git(#[from] hangar_worktree::Error),
    #[error("file watcher: {0}")]
    Notify(#[from] notify::Error),
    #[error("I/O error: {0}")]
    Io(#[from] io::Error),
    #[error("live diff worker stopped")]
    Stopped,
}

pub type Result<T, E = Error> = std::result::Result<T, E>;
