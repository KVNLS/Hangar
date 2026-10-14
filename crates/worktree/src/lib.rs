//! Git worktree management, branch status and diffs via the git CLI (porcelain output only).
//! Every call blocks on a `git` subprocess: run them off the UI thread.

mod diff;
mod error;
pub mod gh;
mod git;
mod ignore;
mod patch;
mod repo;
mod review;
mod status;
mod worktree;

pub use diff::{
    FileStat, diff_file, diff_files, diff_files_with, diff_paths, diff_stat, list_files,
    merge_base, parse_numstat, untracked_files,
};
pub use error::{Error, Result};
pub use gh::{CreatePr, Gh, GhStatus, PrInfo, gh_available};
pub use git::{Git, GitVersion, MIN_GIT_VERSION, git_version};
pub use ignore::{GitDirs, check_ignored, git_dirs};
pub use patch::{DiffLine, DiffOptions, FileDiff, FileStatus, Hunk, LineKind, parse_patch};
pub use repo::{branch_exists, current_branch, default_branch, repo_root};
pub use review::{
    BaseCheckout, CommitOptions, MergeOutcome, MergeStrategy, PushOutcome, Upstream, base_checkout,
    commit, merge_conflicts, merge_into_base, push, remotes, stage, stage_all, unstage, upstream,
};
pub use status::{ChangeKind, ChangedPath, Status, parse_status, status, status_paths};
pub use worktree::{
    CreateOptions, Worktree, create, delete_branch, list, parse_list, prune, remove,
};
