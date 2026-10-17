//! The workspace lifecycle on a real repository and SQLite store: what the use cases (tested with
//! fakes in `hangar-usecases`) do once wired to `git`, through the `ops` the app calls.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::Path;
use std::process::Command;

use hangar_app::agents;
use hangar_app::ops::{
    DeleteOptions, DeleteSummary, NewWorkspace, add_project, archive_workspace,
    create_workspace_from, delete_summary, delete_workspace, load, remove_project,
    restore_workspace, sessions_ended_since,
};
use hangar_core::{
    Project, Session, SessionStatus, Timestamp, Workspace, WorkspaceId, WorkspaceStatus,
    branch_name,
};
use hangar_store_sqlite::Store;

fn git(dir: &Path, args: &[&str]) {
    let status = Command::new("git")
        .args(args)
        .current_dir(dir)
        .status()
        .unwrap();
    assert!(status.success(), "git {args:?}");
}

fn repo() -> tempfile::TempDir {
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
fn commit_all(dir: &Path, message: &str) {
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
fn branch_exists(repo: &Path, branch: &str) -> bool {
    hangar_worktree::branch_exists(repo, branch).unwrap()
}

struct Fixture {
    repo: tempfile::TempDir,
    root: tempfile::TempDir,
    store: Store,
    project: Project,
}

fn fixture() -> Fixture {
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
        let ws = create_workspace_from(
            &self.store,
            &self.project,
            self.root.path(),
            &NewWorkspace::named(name),
        )
        .unwrap();
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

#[test]
fn add_project_resolves_root_and_is_idempotent() {
    let repo = repo();
    let sub = repo.path().join("sub");
    std::fs::create_dir(&sub).unwrap();
    let store = Store::open_in_memory().unwrap();
    let project = add_project(&store, &sub).unwrap();
    assert_eq!(
        project.repo_path.canonicalize().unwrap(),
        repo.path().canonicalize().unwrap()
    );
    assert_eq!(project.default_base, "main");
    assert_eq!(add_project(&store, repo.path()).unwrap().id, project.id);
    assert_eq!(store.list_projects().unwrap().len(), 1);
}

#[test]
fn add_project_refuses_a_repository_without_commits() {
    let dir = tempfile::tempdir().unwrap();
    git(dir.path(), &["init", "-q", "-b", "main"]);
    let store = Store::open_in_memory().unwrap();
    let err = add_project(&store, dir.path()).unwrap_err();
    assert!(format!("{err:#}").contains("no commits yet"), "{err:#}");
    assert!(store.list_projects().unwrap().is_empty());
}

#[test]
fn add_project_with_a_detached_head_never_uses_head_as_base() {
    let repo = tempfile::tempdir().unwrap();
    git(repo.path(), &["init", "-q", "-b", "trunk"]);
    git(
        repo.path(),
        &[
            "-c",
            "user.name=t",
            "-c",
            "user.email=t@t",
            "commit",
            "-q",
            "--allow-empty",
            "-m",
            "a",
        ],
    );
    git(repo.path(), &["checkout", "-q", "--detach"]);
    let store = Store::open_in_memory().unwrap();
    // One local branch: it is the base.
    assert_eq!(
        add_project(&store, repo.path()).unwrap().default_base,
        "trunk"
    );

    let other = tempfile::tempdir().unwrap();
    git(other.path(), &["init", "-q", "-b", "trunk"]);
    git(
        other.path(),
        &[
            "-c",
            "user.name=t",
            "-c",
            "user.email=t@t",
            "commit",
            "-q",
            "--allow-empty",
            "-m",
            "a",
        ],
    );
    git(other.path(), &["branch", "dev"]);
    git(other.path(), &["checkout", "-q", "--detach"]);
    // Several and none of them main/master: the user picks by checking one out.
    let err = add_project(&store, other.path()).unwrap_err();
    assert!(
        format!("{err:#}").contains("check out the branch"),
        "{err:#}"
    );
}

#[test]
fn add_project_rejects_non_repo() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open_in_memory().unwrap();
    assert!(add_project(&store, dir.path()).is_err());
}

#[test]
fn create_workspace_from_a_base_on_a_chosen_branch() {
    let repo = repo();
    let root = tempfile::tempdir().unwrap();
    let store = Store::open_in_memory().unwrap();
    let project = add_project(&store, repo.path()).unwrap();
    let git = |args: &[&str]| {
        let out = std::process::Command::new("git")
            .arg("-C")
            .arg(repo.path())
            .args(args)
            .output()
            .unwrap();
        assert!(out.status.success(), "git {args:?}: {out:?}");
        String::from_utf8_lossy(&out.stdout).trim().to_owned()
    };
    git(&["branch", "release"]);
    let release = git(&["rev-parse", "release"]);

    let ws = create_workspace_from(
        &store,
        &project,
        root.path(),
        &NewWorkspace {
            base: Some("release".into()),
            branch: Some("fix/crash".into()),
            task: Some("  Fix the crash  ".into()),
            ..NewWorkspace::named("Hotfix")
        },
    )
    .unwrap();
    assert_eq!(ws.task.as_deref(), Some("Fix the crash"));
    assert_eq!(store.get_workspace(ws.id).unwrap().task, ws.task);
    assert_eq!(
        (ws.slug.as_str(), ws.branch.as_str()),
        ("hotfix", "fix/crash")
    );
    assert_eq!(ws.base, "release");
    assert_eq!(git(&["rev-parse", "fix/crash"]), release);

    let taken = create_workspace_from(
        &store,
        &project,
        root.path(),
        &NewWorkspace {
            branch: Some("fix/crash".into()),
            ..NewWorkspace::named("again")
        },
    );
    assert!(taken.unwrap_err().to_string().contains("already exists"));
    let bad = create_workspace_from(
        &store,
        &project,
        root.path(),
        &NewWorkspace {
            branch: Some("a..b".into()),
            ..NewWorkspace::named("x")
        },
    );
    assert!(
        bad.unwrap_err()
            .to_string()
            .contains("not a valid branch name")
    );
    // Blank fields fall back to the defaults.
    let ws = create_workspace_from(
        &store,
        &project,
        root.path(),
        &NewWorkspace {
            base: Some(" ".into()),
            branch: Some(String::new()),
            task: Some(" ".into()),
            ..NewWorkspace::named("plain")
        },
    )
    .unwrap();
    assert_eq!(ws.branch, "hangar/plain");
    assert_eq!(ws.task, None);
    assert_eq!(ws.base, project.default_base);
}

#[test]
fn create_workspace_makes_branch_worktree_and_row() {
    let repo = repo();
    let root = tempfile::tempdir().unwrap();
    let store = Store::open_in_memory().unwrap();
    let project = add_project(&store, repo.path()).unwrap();

    let ws = create_workspace_from(
        &store,
        &project,
        root.path(),
        &NewWorkspace::named("Fix Login"),
    )
    .unwrap();
    assert_eq!(ws.slug, "fix-login");
    assert_eq!(ws.branch, "hangar/fix-login");
    assert!(ws.worktree_path.is_dir());
    assert!(
        ws.worktree_path
            .ends_with(format!("{}/fix-login", hangar_core::slugify(&project.name)))
    );

    let again = create_workspace_from(
        &store,
        &project,
        root.path(),
        &NewWorkspace::named("fix login"),
    )
    .unwrap();
    assert_eq!(again.slug, "fix-login-2");
    assert_eq!(again.branch, "hangar/fix-login-2");

    let loaded = load(&store).unwrap();
    assert_eq!(loaded.entries.len(), 1);
    assert_eq!(loaded.entries[0].workspaces.len(), 2);

    assert!(
        create_workspace_from(&store, &project, root.path(), &NewWorkspace::named("   ")).is_err()
    );
}

#[test]
fn archive_restore_and_load_keep_archived_workspaces() {
    let f = fixture();
    let ws = f.workspace("later");
    let session = f.store.list_sessions(ws.id).unwrap()[0].id;
    let stat = hangar_core::changes::DiffStat {
        files: 1,
        insertions: 5,
        deletions: 2,
    };
    archive_workspace(&f.store, ws.id, Some(session), Some(stat)).unwrap();
    assert_eq!(
        f.store.get_workspace(ws.id).unwrap().status,
        WorkspaceStatus::Archived
    );
    let row = f.store.get_session(session).unwrap();
    assert!(matches!(row.status, SessionStatus::Failed { .. }));
    assert_eq!(row.changes, Some(stat));
    let ended = sessions_ended_since(&f.store, hangar_core::Timestamp(0)).unwrap();
    assert_eq!(ended.iter().map(|s| s.id).collect::<Vec<_>>(), [session]);
    assert!(ws.worktree_path.is_dir());

    let loaded = load(&f.store).unwrap();
    assert_eq!(loaded.entries[0].archived().count(), 1);
    assert_eq!(loaded.entries[0].active().count(), 0);

    restore_workspace(&f.store, ws.id).unwrap();
    let loaded = load(&f.store).unwrap();
    assert_eq!(loaded.entries[0].active().count(), 1);
}

#[test]
fn remove_project_requires_no_active_workspaces() {
    let f = fixture();
    let ws = f.workspace("busy");
    let err = remove_project(&f.store, f.project.id).unwrap_err();
    assert!(err.to_string().contains("busy"), "{err}");

    archive_workspace(&f.store, ws.id, None, None).unwrap();
    remove_project(&f.store, f.project.id).unwrap();
    assert!(f.store.list_projects().unwrap().is_empty());
    // The repository and the archived worktree stay on disk.
    assert!(f.repo.path().join(".git").is_dir());
    assert!(ws.worktree_path.is_dir());
}

#[test]
fn delete_clean_workspace_keeps_branch_by_default() {
    let f = fixture();
    let ws = f.workspace("clean");
    let summary = delete_summary(&ws).unwrap();
    assert_eq!(
        summary,
        DeleteSummary {
            exists: true,
            uncommitted: 0,
            untracked: 0,
            ahead: Some(0),
            submodules: 0,
        }
    );

    let outcome = delete_workspace(&f.store, ws.id, DeleteOptions::default()).unwrap();
    assert!(outcome.branch_error.is_none());
    assert!(!ws.worktree_path.exists());
    assert!(branch_exists(f.repo.path(), &ws.branch));
    f.assert_rows_gone(&ws);
    // The repository and the worktree root are untouched.
    assert!(f.repo.path().join(".git").is_dir());
    assert!(f.root.path().is_dir());
    assert!(
        hangar_worktree::list(f.repo.path())
            .unwrap()
            .iter()
            .all(|wt| wt.is_main)
    );
}

#[test]
fn delete_can_delete_a_merged_branch() {
    let f = fixture();
    let ws = f.workspace("merged");
    let options = DeleteOptions {
        force: false,
        delete_branch: true,
    };
    let outcome = delete_workspace(&f.store, ws.id, options).unwrap();
    assert!(outcome.branch_error.is_none());
    assert!(!ws.worktree_path.exists());
    assert!(!branch_exists(f.repo.path(), &ws.branch));
    f.assert_rows_gone(&ws);
}

#[test]
fn delete_keeps_an_unmerged_branch_and_reports_why() {
    let f = fixture();
    let ws = f.workspace("work");
    std::fs::write(ws.worktree_path.join("new.txt"), "x").unwrap();
    commit_all(&ws.worktree_path, "work");
    assert_eq!(delete_summary(&ws).unwrap().ahead, Some(1));

    let options = DeleteOptions {
        force: false,
        delete_branch: true,
    };
    let outcome = delete_workspace(&f.store, ws.id, options).unwrap();
    let error = outcome.branch_error.unwrap();
    assert!(error.contains("hangar/work"), "{error}");
    assert!(branch_exists(f.repo.path(), &ws.branch));
    assert!(!ws.worktree_path.exists());
    f.assert_rows_gone(&ws);
}

#[test]
fn dirty_workspace_needs_force() {
    let f = fixture();
    let ws = f.workspace("dirty");
    std::fs::write(ws.worktree_path.join("scratch.txt"), "x").unwrap();
    let summary = delete_summary(&ws).unwrap();
    assert_eq!((summary.uncommitted, summary.untracked), (0, 1));
    assert!(summary.is_dirty());

    assert!(delete_workspace(&f.store, ws.id, DeleteOptions::default()).is_err());
    assert!(ws.worktree_path.join("scratch.txt").exists());
    assert!(f.store.get_workspace(ws.id).is_ok());

    let force = DeleteOptions {
        force: true,
        delete_branch: false,
    };
    delete_workspace(&f.store, ws.id, force).unwrap();
    assert!(!ws.worktree_path.exists());
    f.assert_rows_gone(&ws);
}

#[test]
fn delete_of_missing_directory_only_prunes_and_forgets() {
    let f = fixture();
    let ws = f.workspace("gone");
    std::fs::remove_dir_all(&ws.worktree_path).unwrap();
    assert_eq!(delete_summary(&ws).unwrap(), DeleteSummary::default());
    delete_workspace(&f.store, ws.id, DeleteOptions::default()).unwrap();
    f.assert_rows_gone(&ws);
    assert_eq!(hangar_worktree::list(f.repo.path()).unwrap().len(), 1);
}

#[test]
fn delete_refuses_paths_that_are_not_linked_worktrees() {
    let f = fixture();
    let outside = tempfile::tempdir().unwrap();
    std::fs::write(outside.path().join("keep.txt"), "x").unwrap();
    for (slug, path) in [
        ("main", f.repo.path().to_path_buf()),
        ("outside", outside.path().to_path_buf()),
    ] {
        let ws = Workspace {
            id: WorkspaceId::new(),
            project_id: f.project.id,
            name: slug.into(),
            slug: slug.into(),
            branch: branch_name(slug),
            base: "main".into(),
            worktree_path: path.clone(),
            status: WorkspaceStatus::Active,
            created_at: Timestamp::now(),
            task: None,
            group: None,
            parent: None,
        };
        f.store.insert_workspace(&ws).unwrap();
        let force = DeleteOptions {
            force: true,
            delete_branch: true,
        };
        let err = delete_workspace(&f.store, ws.id, force).unwrap_err();
        assert!(format!("{err:#}").contains("refusing"), "{err:#}");
        assert!(f.store.get_workspace(ws.id).is_ok());
    }
    assert!(outside.path().join("keep.txt").exists());
    assert!(f.repo.path().join(".git").is_dir());
}
