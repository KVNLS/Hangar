use super::*;
use crate::agents;
use hangar_core::{SessionStatus, Setting, Timestamp, WorkspaceStatus, branch_name};
use hangar_store_sqlite::Store;
use hangar_terminal::ExitInfo;
use std::path::Path;
use std::process::Command;
use std::sync::Arc;
use std::time::Duration;

pub(super) fn git(dir: &Path, args: &[&str]) {
    let status = Command::new("git")
        .args(args)
        .current_dir(dir)
        .status()
        .unwrap();
    assert!(status.success(), "git {args:?}");
}

pub(super) fn repo() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    git(dir.path(), &["init", "-q", "-b", "main"]);
    git(
        dir.path(),
        &[
            "-c",
            "user.name=t",
            "-c",
            "user.email=t@t",
            "commit",
            "-q",
            "--allow-empty",
            "-m",
            "init",
        ],
    );
    dir
}

pub(super) fn commit_all(dir: &Path, message: &str) {
    git(dir, &["add", "-A"]);
    git(
        dir,
        &[
            "-c",
            "user.name=t",
            "-c",
            "user.email=t@t",
            "commit",
            "-q",
            "-m",
            message,
        ],
    );
}

pub(super) fn branch_exists(repo: &Path, branch: &str) -> bool {
    hangar_worktree::branch_exists(repo, branch).unwrap()
}

pub(super) struct Fixture {
    repo: tempfile::TempDir,
    root: tempfile::TempDir,
    store: Store,
    project: Project,
}

pub(super) fn fixture() -> Fixture {
    let repo = repo();
    let root = tempfile::tempdir().unwrap();
    let store = Store::open_in_memory().unwrap();
    let project = add_project(&store, repo.path()).unwrap();
    Fixture {
        repo,
        root,
        store,
        project,
    }
}

impl Fixture {
    fn workspace(&self, name: &str) -> Workspace {
        let ws = create_workspace(&self.store, &self.project, name, self.root.path()).unwrap();
        self.store
            .insert_session(&Session::new(ws.id, agents::kind("shell")))
            .unwrap();
        ws
    }

    fn assert_rows_gone(&self, ws: &Workspace) {
        assert!(self.store.get_workspace(ws.id).is_err());
        assert!(self.store.list_sessions(ws.id).unwrap().is_empty());
    }
}

mod add_project;
mod archive_project;
mod create_workspace;
mod delete;
mod modes_queue;
mod sessions;
mod settings;
