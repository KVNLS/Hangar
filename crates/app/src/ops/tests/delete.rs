use super::*;

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

#[test]
fn delete_summary_description() {
    let clean = DeleteSummary {
        exists: true,
        ahead: Some(0),
        ..Default::default()
    };
    assert_eq!(
        clean.describe("main"),
        [
            "No uncommitted or untracked files.",
            "No commits ahead of main."
        ]
    );
    let dirty = DeleteSummary {
        exists: true,
        uncommitted: 2,
        untracked: 1,
        ahead: Some(3),
        submodules: 0,
    };
    assert_eq!(
        dirty.describe("main"),
        [
            "2 uncommitted and 1 untracked file(s) will be lost.",
            "3 commit(s) ahead of main."
        ]
    );
    assert_eq!(DeleteSummary::default().describe("main").len(), 1);
}
