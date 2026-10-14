use super::*;

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
