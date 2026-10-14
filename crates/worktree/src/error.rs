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
    #[error("{}", hangar_core::review::push_rejected_message(.remote, .branch, .reason, *.non_fast_forward))]
    PushRejected {
        remote: String,
        branch: String,
        /// git's short reason, e.g. `non-fast-forward`, `fetch first`, `pre-receive hook declined`.
        reason: String,
        /// The remote branch has commits the local one lacks: pull or rebase, then push again.
        non_fast_forward: bool,
    },
    #[error("`{0}` has no upstream branch to pull from: push it first")]
    NoUpstream(String),
    /// Rebasing onto the upstream conflicted; the rebase was aborted and nothing changed.
    #[error("rebasing onto `{onto}` would conflict in: {}", .paths.iter().map(|p| p.display().to_string()).collect::<Vec<_>>().join(", "))]
    RebaseConflicts { onto: String, paths: Vec<PathBuf> },
    /// The pull went through but the uncommitted changes put back on top conflict with it:
    /// the files hold conflict markers and the changes are also kept in `git stash list`.
    #[error("pulled {pulled} commit(s) from `{onto}`, but your uncommitted changes conflict with them in: {}; resolve the conflict markers (your changes are also saved in `git stash list`)", .paths.iter().map(|p| p.display().to_string()).collect::<Vec<_>>().join(", "))]
    PulledWithConflicts {
        onto: String,
        pulled: u32,
        paths: Vec<PathBuf>,
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
    #[error("`git {}` did not finish within {secs} s (waiting for a password or network?)", .args.join(" "))]
    Timeout { args: Vec<String>, secs: u64 },
    #[error("pull requests need a GitHub remote: none of this repository's remotes is on GitHub")]
    NotGitHub,
    #[error("`gh` did not answer within {secs} s")]
    GhTimeout { secs: u64 },
    #[error("unexpected git output: {0}")]
    Parse(String),
    #[error("I/O error running git: {0}")]
    Io(#[from] io::Error),
}

pub type Result<T, E = Error> = std::result::Result<T, E>;
