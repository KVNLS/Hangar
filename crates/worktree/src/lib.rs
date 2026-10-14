//! Git worktree management, branch status and diffs via the git CLI (porcelain output only).
//! Every call blocks on a `git` subprocess: run them off the UI thread.

mod adapter;
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

pub use adapter::GitCli;
pub use diff::{
    FileStat, diff_file, diff_files, diff_files_with, diff_paths, diff_stat, list_files,
    merge_base, parse_numstat, untracked_files,
};
pub use error::{Error, Result};
pub use gh::{Gh, gh_available};
pub use git::{Git, GitVersion, MIN_GIT_VERSION, git_version, identity};
pub use hangar_core::changes::{
    ChangeKind, ChangedPath, DiffLine, DiffOptions, FileDiff, FileStatus, GitDirs, Hunk, LineKind,
    Status,
};
pub use hangar_core::review::{
    CreatePr, GhStatus, MergeOutcome, MergeStrategy, PrInfo, PullOutcome, PushOutcome, Upstream,
};
pub use ignore::{check_ignored, git_dirs};
pub use patch::parse_patch;
pub use repo::{
    base_branch_name, branch_exists, branch_tips, current_branch, default_branch,
    is_valid_branch_name, local_branches, repo_root,
};
pub use review::{
    BaseCheckout, CommitOptions, base_checkout, commit, merge_conflicts, merge_into_base,
    pull_rebase, push, remotes, stage, stage_all, unstage, upstream,
};
pub use status::{initialized_submodules, parse_status, status, status_paths};
pub use worktree::{
    CreateOptions, Worktree, create, delete_branch, list, parse_list, prune, remove,
};
