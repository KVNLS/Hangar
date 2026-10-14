//! Reviewing a workspace: publishing its branch, merging it and its pull request, as plain data.

use std::path::PathBuf;

use crate::ports::PortError;

/// The branch's upstream and how far the branch is from it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Upstream {
    /// Short name, e.g. `origin/hangar/fix-login`.
    pub name: String,
    pub remote: String,
    /// Full ref on the remote, e.g. `refs/heads/hangar/fix-login`.
    pub remote_ref: String,
    pub ahead: u32,
    pub behind: u32,
    /// The upstream branch was deleted on the remote (after a fetch).
    pub gone: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PushOutcome {
    pub remote: String,
    pub branch: String,
    /// This push set the branch's upstream (first push).
    pub set_upstream: bool,
    /// The remote already had this commit.
    pub up_to_date: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PullOutcome {
    /// The upstream rebased onto, e.g. `origin/hangar/fix-login`.
    pub upstream: String,
    /// Commits taken from the upstream.
    pub pulled: u32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MergeStrategy {
    /// `git merge --no-ff`: a merge commit keeps the branch's history.
    NoFf,
    /// `git merge --squash` then one commit.
    Squash,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MergeOutcome {
    /// Checkout where `base` is checked out and the merge was made.
    pub worktree: PathBuf,
    /// The new commit on `base`.
    pub head: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum GhStatus {
    Installed {
        authenticated: bool,
        /// Logged-in account, when it could be read.
        account: Option<String>,
    },
    Missing,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PrInfo {
    pub number: u64,
    pub url: String,
    /// `OPEN`, `CLOSED` or `MERGED`.
    pub state: String,
    pub title: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CreatePr {
    pub base: String,
    /// Branch the PR is opened from; must already be pushed.
    pub head: String,
    pub title: String,
    pub body: String,
    pub draft: bool,
}

/// Why a review action was refused or failed. The refusals are part of the domain (the UI
/// explains them and offers a way out); anything else is an adapter's [`PortError`].
#[derive(Debug, thiserror::Error)]
pub enum ReviewError {
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
    #[error("`{0}` has no upstream branch to pull from: push it first")]
    NoUpstream(String),
    /// Rebasing onto the upstream conflicted; the rebase was aborted and nothing changed.
    #[error("rebasing onto `{onto}` would conflict in: {}", join(.paths))]
    RebaseConflicts { onto: String, paths: Vec<PathBuf> },
    /// The pull went through but the uncommitted changes put back on top conflict with it.
    #[error(
        "pulled {pulled} commit(s) from `{onto}`, but your uncommitted changes conflict with \
         them in: {}; resolve the conflict markers (your changes are also saved in `git stash \
         list`)",
        join(.paths)
    )]
    PulledWithConflicts {
        onto: String,
        pulled: u32,
        paths: Vec<PathBuf>,
    },
    /// The base branch cannot receive a merge right now (not checked out, dirty, mid-merge).
    #[error("cannot merge into the base branch: {reason}")]
    BaseNotReady { reason: String },
    #[error("merging `{branch}` into `{base}` would conflict in: {}", join(.paths))]
    MergeConflicts {
        branch: String,
        base: String,
        paths: Vec<PathBuf>,
    },
    #[error("`{branch}` is already merged into `{base}`")]
    AlreadyMerged { branch: String, base: String },
    #[error("the GitHub CLI (`gh`) is not installed or not on PATH")]
    GhMissing,
    #[error("the GitHub CLI (`gh`) did not answer in time (offline?)")]
    GhNotAnswering,
    #[error("the pull request title is empty")]
    EmptyPrTitle,
    #[error("failed to push before opening the pull request")]
    PushBeforePr(#[source] Box<ReviewError>),
    #[error(transparent)]
    Other(#[from] PortError),
}

pub fn push_rejected_message(remote: &str, branch: &str, reason: &str, non_ff: bool) -> String {
    if non_ff {
        format!(
            "push to {remote} rejected: {remote}/{branch} has commits that are not in \
             {branch} ({reason}); pull or rebase, then push again"
        )
    } else {
        format!("push of {branch} to {remote} rejected: {reason}")
    }
}

fn join(paths: &[PathBuf]) -> String {
    paths
        .iter()
        .map(|p| p.display().to_string())
        .collect::<Vec<_>>()
        .join(", ")
}
