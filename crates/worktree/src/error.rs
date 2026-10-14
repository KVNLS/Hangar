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
    #[error("nothing to commit")]
    NothingToCommit,
    #[error("the commit message is empty")]
    EmptyMessage,
    /// A commit hook (pre-commit, commit-msg, ...) exited non-zero; `stderr` is its output.
    #[error("a git hook rejected the commit: {stderr}")]
    HookFailed { stderr: String },
    #[error("HEAD is detached: check out a branch first")]
    DetachedHead,
    #[error("branch `{0}` does not exist")]
    BranchMissing(String),
    #[error("no remote to push to: add one (e.g. `origin`) first")]
    NoRemote,
    #[error("{}", push_rejected_message(.remote, .branch, .reason, *.non_fast_forward))]
    PushRejected {
        remote: String,
        branch: String,
        /// git's short reason, e.g. `non-fast-forward`, `fetch first`, `pre-receive hook declined`.
        reason: String,
        /// The remote branch has commits the local one lacks: pull or rebase, then push again.
        non_fast_forward: bool,
    },
    /// The base branch cannot receive a merge right now (not checked out, dirty, mid-merge).
    #[error("cannot merge into the base branch: {reason}")]
    BaseNotReady { reason: String },
    #[error("merging `{branch}` into `{base}` would conflict in: {}", .paths.iter().map(|p| p.display().to_string()).collect::<Vec<_>>().join(", "))]
    MergeConflicts {
        branch: String,
        base: String,
        paths: Vec<PathBuf>,
    },
    #[error("`{branch}` is already merged into `{base}`")]
    AlreadyMerged { branch: String, base: String },
    #[error("the GitHub CLI (`gh`) is not installed or not on PATH")]
    GhMissing,
    #[error("`gh {}` failed ({status}): {stderr}", .args.join(" "))]
    Gh {
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

fn push_rejected_message(remote: &str, branch: &str, reason: &str, non_ff: bool) -> String {
    if non_ff {
        format!(
            "push to {remote} rejected: {remote}/{branch} has commits that are not in \
             {branch} ({reason}); pull or rebase, then push again"
        )
    } else {
        format!("push of {branch} to {remote} rejected: {reason}")
    }
}
