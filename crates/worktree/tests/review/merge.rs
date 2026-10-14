use super::*;

fn assert_base_untouched(fx: &Fixture, head: &str) {
    assert_eq!(fx.head(&fx.repo), head);
    assert!(status(&fx.repo).unwrap().is_clean());
    assert!(!fx.repo.join(".git/MERGE_HEAD").exists());
}

#[test]
fn merge_no_ff_into_main_checkout() {
    let fx = Fixture::new();
    fx.commit_in_wt("a.txt", "a\n", "a");
    fx.commit_in_wt("b.txt", "b\n", "b");
    let before = fx.head(&fx.repo);
    let out = merge_into_base(
        &fx.repo,
        "hangar/task",
        "main",
        MergeStrategy::NoFf,
        "Merge task",
    )
    .unwrap();
    assert_eq!(out.worktree, fx.repo);
    assert_eq!(out.head, fx.head(&fx.repo));
    let parents = git(&fx.repo, &["log", "-1", "--format=%P"]);
    assert_eq!(parents.split_whitespace().count(), 2);
    assert_eq!(parents.split_whitespace().next().unwrap(), before);
    assert_eq!(
        git(&fx.repo, &["log", "-1", "--format=%s"]).trim(),
        "Merge task"
    );
    assert!(fx.repo.join("b.txt").exists());
    assert!(status(&fx.repo).unwrap().is_clean());

    assert!(matches!(
        merge_into_base(
            &fx.repo,
            "hangar/task",
            "main",
            MergeStrategy::NoFf,
            "again"
        ),
        Err(Error::AlreadyMerged { .. })
    ));
}

#[test]
fn merge_squash_into_main_checkout() {
    let fx = Fixture::new();
    fx.commit_in_wt("a.txt", "a\n", "a");
    fx.commit_in_wt("b.txt", "b\n", "b");
    let before = fx.head(&fx.repo);
    let out = merge_into_base(
        &fx.repo,
        "hangar/task",
        "main",
        MergeStrategy::Squash,
        "Squashed task",
    )
    .unwrap();
    assert_eq!(out.head, fx.head(&fx.repo));
    assert_eq!(git(&fx.repo, &["log", "-1", "--format=%P"]).trim(), before);
    assert_eq!(
        git(&fx.repo, &["log", "-1", "--format=%s"]).trim(),
        "Squashed task"
    );
    assert!(fx.repo.join("a.txt").exists() && fx.repo.join("b.txt").exists());
    assert!(status(&fx.repo).unwrap().is_clean());
}

#[test]
fn merge_squash_rolls_back_when_the_commit_hook_fails() {
    let fx = Fixture::new();
    fx.commit_in_wt("a.txt", "a\n", "a");
    let before = fx.head(&fx.repo);
    fx.hook("pre-commit", "echo 'nope' >&2; exit 1");
    let err =
        merge_into_base(&fx.repo, "hangar/task", "main", MergeStrategy::Squash, "s").unwrap_err();
    assert!(matches!(err, Error::HookFailed { .. }), "{err:?}");
    assert_base_untouched(&fx, &before);
    assert!(!fx.repo.join("a.txt").exists());

    fs::remove_file(fx.hooks.join("pre-commit")).unwrap();
    fx.hook("pre-merge-commit", "echo 'nope' >&2; exit 1");
    let err =
        merge_into_base(&fx.repo, "hangar/task", "main", MergeStrategy::NoFf, "m").unwrap_err();
    assert!(matches!(err, Error::HookFailed { .. }), "{err:?}");
    assert_base_untouched(&fx, &before);
}

#[test]
fn merge_refuses_when_base_is_dirty() {
    let fx = Fixture::new();
    fx.commit_in_wt("a.txt", "a\n", "a");
    let before = fx.head(&fx.repo);
    fx.write(&fx.repo, "README.md", "local edit\n");
    let err =
        merge_into_base(&fx.repo, "hangar/task", "main", MergeStrategy::NoFf, "m").unwrap_err();
    assert!(matches!(err, Error::BaseNotReady { .. }), "{err:?}");
    assert_eq!(fx.head(&fx.repo), before);
    assert_eq!(
        fs::read_to_string(fx.repo.join("README.md")).unwrap(),
        "local edit\n"
    );
    assert_eq!(status(&fx.repo).unwrap().changes.len(), 1);

    git(&fx.repo, &["checkout", "-q", "README.md"]);
    fx.write(&fx.repo, "stray.txt", "s\n");
    let err =
        merge_into_base(&fx.repo, "hangar/task", "main", MergeStrategy::Squash, "m").unwrap_err();
    assert!(matches!(err, Error::BaseNotReady { .. }), "{err:?}");
    assert_eq!(fx.head(&fx.repo), before);
}

#[test]
fn merge_refuses_when_base_is_not_checked_out() {
    let fx = Fixture::new();
    fx.commit_in_wt("a.txt", "a\n", "a");
    git(&fx.repo, &["checkout", "-q", "-b", "elsewhere"]);
    let before = fx.head(&fx.repo);
    let err =
        merge_into_base(&fx.repo, "hangar/task", "main", MergeStrategy::NoFf, "m").unwrap_err();
    match &err {
        Error::BaseNotReady { reason } => assert!(reason.contains("not checked out"), "{reason}"),
        other => panic!("{other:?}"),
    }
    assert_eq!(fx.head(&fx.repo), before);
    assert_eq!(
        git(&fx.repo, &["symbolic-ref", "--short", "HEAD"]).trim(),
        "elsewhere"
    );
    assert!(matches!(
        merge_into_base(&fx.repo, "hangar/task", "nope", MergeStrategy::NoFf, "m"),
        Err(Error::BaseNotReady { .. })
    ));
    assert!(matches!(
        merge_into_base(&fx.repo, "hangar/missing", "main", MergeStrategy::NoFf, "m"),
        Err(Error::BranchMissing(_))
    ));

    // Base checked out in a linked worktree instead works.
    let main_wt = fx.root.join("worktrees").join("main");
    git(
        &fx.repo,
        &["worktree", "add", "-q", main_wt.to_str().unwrap(), "main"],
    );
    let found = base_checkout(&fx.repo, "main").unwrap();
    assert_eq!(found.path, main_wt);
    assert!(!found.is_main);
    let out = merge_into_base(&fx.repo, "hangar/task", "main", MergeStrategy::NoFf, "m").unwrap();
    assert_eq!(out.worktree, main_wt);
}

#[test]
fn merge_refuses_conflicts_without_touching_base() {
    let fx = Fixture::new();
    fx.commit_in_wt("README.md", "ours in task\n", "task edit");
    fx.commit_in_wt("lib.rs", "fn task() {}\n", "task lib");
    fx.write(&fx.repo, "README.md", "theirs on main\n");
    fx.write(&fx.repo, "lib.rs", "fn main_side() {}\n");
    git(&fx.repo, &["commit", "-q", "-am", "main edit"]);
    let before = fx.head(&fx.repo);
    for strategy in [MergeStrategy::NoFf, MergeStrategy::Squash] {
        match merge_into_base(&fx.repo, "hangar/task", "main", strategy, "m") {
            Err(Error::MergeConflicts { paths, .. }) => {
                assert_eq!(paths, [p("README.md"), p("lib.rs")]);
            }
            other => panic!("expected MergeConflicts, got {other:?}"),
        }
        assert_base_untouched(&fx, &before);
    }
}
