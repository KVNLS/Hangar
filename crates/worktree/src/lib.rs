//! Git worktree management, branch status and diffs via the git CLI (porcelain output only).
//! Every call blocks on a `git` subprocess: run them off the UI thread.

mod diff;
mod error;
mod git;
mod repo;
mod status;
mod worktree;

pub use diff::{FileStat, diff_stat, parse_numstat};
pub use error::{Error, Result};
pub use git::{Git, GitVersion, MIN_GIT_VERSION, git_version};
pub use repo::{branch_exists, current_branch, default_branch, repo_root};
pub use status::{ChangeKind, ChangedPath, Status, parse_status, status};
pub use worktree::{
    CreateOptions, Worktree, create, delete_branch, list, parse_list, prune, remove,
};
