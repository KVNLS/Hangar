use super::*;

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

    let ws = create_workspace(&store, &project, "Fix Login", root.path()).unwrap();
    assert_eq!(ws.slug, "fix-login");
    assert_eq!(ws.branch, "hangar/fix-login");
    assert!(ws.worktree_path.is_dir());
    assert!(
        ws.worktree_path
            .ends_with(format!("{}/fix-login", hangar_core::slugify(&project.name)))
    );

    let again = create_workspace(&store, &project, "fix login", root.path()).unwrap();
    assert_eq!(again.slug, "fix-login-2");
    assert_eq!(again.branch, "hangar/fix-login-2");

    let loaded = load(&store).unwrap();
    assert_eq!(loaded.entries.len(), 1);
    assert_eq!(loaded.entries[0].workspaces.len(), 2);

    assert!(create_workspace(&store, &project, "   ", root.path()).is_err());
}
