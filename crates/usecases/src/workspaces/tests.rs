use hangar_core::ports::ListedWorktree;

use super::*;
use crate::fakes::{FakeGit, FakeStore};

fn project(store: &FakeStore) -> Project {
    let project = Project::new("My App", PathBuf::from("/repo"), "main");
    store.insert_project(&project).unwrap();
    project
}

fn root() -> PathBuf {
    PathBuf::from("/nonexistent/hangar-usecases-tests")
}

#[test]
fn create_names_branch_and_worktree_from_the_slug() {
    let (store, git) = (
        FakeStore::default(),
        FakeGit::with_branches(&["hangar/fix-login"]),
    );
    let project = project(&store);
    let ws = Workspaces::new(&store, &git)
        .create(&project, &root(), &NewWorkspace::named(" Fix login "))
        .unwrap();
    assert_eq!(
        (ws.name.as_str(), ws.slug.as_str()),
        ("Fix login", "fix-login-2")
    );
    assert_eq!(ws.branch, "hangar/fix-login-2");
    assert_eq!(ws.worktree_path, root().join("my-app/fix-login-2"));
    assert_eq!(git.calls(), ["add hangar/fix-login-2 from main"]);
    assert_eq!(store.list_workspaces(project.id).unwrap(), [ws]);
}

#[test]
fn archive_stops_the_session_with_the_workspace_diff() {
    let (store, git) = (FakeStore::default(), FakeGit::default());
    let project = project(&store);
    let workspaces = Workspaces::new(&store, &git);
    let ws = workspaces
        .create(&project, &root(), &NewWorkspace::named("Docs"))
        .unwrap();
    let session = hangar_core::Session::new(ws.id, hangar_core::AgentKind::Claude);
    store.insert_session(&session).unwrap();
    let stat = DiffStat {
        files: 1,
        insertions: 2,
        deletions: 3,
    };
    workspaces
        .archive(ws.id, Some(session.id), Some(stat))
        .unwrap();
    assert_eq!(
        store.get_workspace(ws.id).unwrap().status,
        WorkspaceStatus::Archived
    );
    let recorded = store.list_sessions(ws.id).unwrap().remove(0);
    assert!(crate::sessions::is_stopped(&recorded.status));
    assert_eq!(recorded.changes, Some(stat));
}

#[test]
fn create_rolls_back_branch_and_worktree_when_the_row_cannot_be_saved() {
    let store = FakeStore {
        refuse_workspaces: true,
        ..FakeStore::default()
    };
    let git = FakeGit::default();
    let project = project(&store);
    let err = Workspaces::new(&store, &git)
        .create(
            &project,
            &root(),
            &NewWorkspace {
                base: Some("develop".into()),
                ..NewWorkspace::named("Fix")
            },
        )
        .unwrap_err();
    assert!(matches!(err, WorkspaceError::SaveWorkspace(_)), "{err}");
    let path = root().join("my-app/fix");
    assert_eq!(
        git.calls(),
        [
            "add hangar/fix from develop".to_owned(),
            format!("remove {} force=true", path.display()),
            "delete-branch hangar/fix force=true".to_owned(),
        ]
    );
    assert!(!git.branch_exists(&project.repo_path, "hangar/fix").unwrap());
}

#[test]
fn create_refuses_bad_or_taken_branch_names_before_touching_git() {
    let (store, git) = (FakeStore::default(), FakeGit::with_branches(&["taken"]));
    let project = project(&store);
    let workspaces = Workspaces::new(&store, &git);
    let err = workspaces
        .create(
            &project,
            &root(),
            &NewWorkspace {
                branch: Some("bad name".into()),
                ..NewWorkspace::named("x")
            },
        )
        .unwrap_err();
    assert_eq!(err.to_string(), "`bad name` is not a valid branch name");
    let err = workspaces
        .create(
            &project,
            &root(),
            &NewWorkspace {
                branch: Some("taken".into()),
                ..NewWorkspace::named("x")
            },
        )
        .unwrap_err();
    assert_eq!(err.to_string(), "branch `taken` already exists");
    let err = workspaces
        .create(&project, &root(), &NewWorkspace::named("  "))
        .unwrap_err();
    assert_eq!(err.to_string(), "workspace name is empty");
    assert!(git.calls().is_empty());
}

#[test]
fn delete_refuses_the_main_worktree() {
    let main = std::env::temp_dir();
    let git = FakeGit {
        listed: vec![ListedWorktree {
            path: main.clone(),
            is_main: true,
            bare: false,
        }],
        ..FakeGit::default()
    };
    let store = FakeStore::default();
    let project = project(&store);
    let ws = Workspace {
        id: WorkspaceId::new(),
        project_id: project.id,
        name: "main".into(),
        slug: "main".into(),
        branch: "main".into(),
        base: "main".into(),
        worktree_path: main.clone(),
        status: WorkspaceStatus::Active,
        created_at: Timestamp::now(),
        task: None,
        group: None,
        parent: None,
    };
    store.insert_workspace(&ws).unwrap();
    let err = Workspaces::new(&store, &git)
        .delete(ws.id, DeleteOptions::default())
        .unwrap_err();
    assert!(matches!(err, WorkspaceError::MainWorktree(_)), "{err}");
    assert!(git.calls().is_empty());
    assert_eq!(store.list_workspaces(project.id).unwrap().len(), 1);
}

#[test]
fn remove_project_lists_the_active_workspaces() {
    let (store, git) = (FakeStore::default(), FakeGit::default());
    let project = project(&store);
    let workspaces = Workspaces::new(&store, &git);
    workspaces
        .create(&project, &root(), &NewWorkspace::named("One"))
        .unwrap();
    let err = workspaces.remove_project(project.id).unwrap_err();
    assert_eq!(
        err.to_string(),
        "archive or delete its workspaces first: One"
    );
}
