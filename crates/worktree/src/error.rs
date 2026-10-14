use std::io;
use std::path::PathBuf;
use std::process::ExitStatus;

use crate::git::GitVersion;

/// Errors raised while running or parsing the git CLI.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("git executable not found on PATH")]
    GitMissing,
    #[error("git {found} is too old; {required} or newer is required")]
    GitTooOld {
        found: GitVersion,
        required: GitVersion,
    },
    #[error("not a git repository: {}", .0.display())]
    NotARepo(PathBuf),
    #[error("branch `{0}` already exists")]
    BranchExists(String),
    #[error("`git {}` failed ({status}): {stderr}", .args.join(" "))]
    Git {
        args: Vec<String>,
        status: ExitStatus,
        stderr: String,
    },
    #[error("unexpected git output: {0}")]
    Parse(String),
    #[error("I/O error running git: {0}")]
    Io(#[from] io::Error),
}

pub type Result<T, E = Error> = std::result::Result<T, E>;
