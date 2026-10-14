//! Git worktree management, branch status and diffs via the git CLI (porcelain output only).
//! Every call blocks on a `git` subprocess: run them off the UI thread.

mod diff;
mod error;
mod git;
mod ignore;
mod patch;
mod repo;
mod status;
mod worktree;

pub use diff::{
    FileStat, diff_file, diff_files, diff_files_with, diff_paths, diff_stat, merge_base,
    parse_numstat, untracked_files,
};
pub use error::{Error, Result};
pub use git::{Git, GitVersion, MIN_GIT_VERSION, git_version};
pub use ignore::{GitDirs, check_ignored, git_dirs};
pub use patch::{DiffLine, DiffOptions, FileDiff, FileStatus, Hunk, LineKind, parse_patch};
pub use repo::{branch_exists, current_branch, default_branch, repo_root};
pub use status::{ChangeKind, ChangedPath, Status, parse_status, status, status_paths};
pub use worktree::{
    CreateOptions, Worktree, create, delete_branch, list, parse_list, prune, remove,
};
