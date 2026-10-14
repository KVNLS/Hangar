//! Stage, commit, push, merge and the `gh` wrapper against temporary repositories with a local
//! bare `origin` and a fake `gh` script. Nothing here talks to a real remote.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;

use hangar_worktree::{
    CommitOptions, CreateOptions, CreatePr, Error, Gh, GhStatus, MergeStrategy, PrInfo,
    base_checkout, commit, create, merge_into_base, pull_rebase, push, remotes, stage, stage_all,
    status, unstage, upstream,
};
use tempfile::TempDir;

mod fixture;
mod gh;
mod merge;
mod push_pull;
mod stage_commit;

use fixture::*;
