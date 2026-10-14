use super::*;

#[test]
fn push_sets_upstream_then_pushes_and_reports_rejection() {
    let fx = Fixture::new();
    assert_eq!(remotes(&fx.repo).unwrap(), ["origin"]);
    assert_eq!(upstream(&fx.wt).unwrap(), None);
    let first = fx.commit_in_wt("a.txt", "a\n", "a");

    let out = push(&fx.wt, None).unwrap();
    assert!(out.set_upstream && !out.up_to_date);
    assert_eq!(
        (out.remote.as_str(), out.branch.as_str()),
        ("origin", "hangar/task")
    );
    assert_eq!(git(&fx.origin, &["rev-parse", "hangar/task"]).trim(), first);
    let up = upstream(&fx.wt).unwrap().unwrap();
    assert_eq!(up.name, "origin/hangar/task");
    assert_eq!((up.ahead, up.behind), (0, 0));

    let second = fx.commit_in_wt("b.txt", "b\n", "b");
    assert_eq!(upstream(&fx.wt).unwrap().unwrap().ahead, 1);
    let out = push(&fx.wt, None).unwrap();
    assert!(!out.set_upstream && !out.up_to_date);
    assert_eq!(
        git(&fx.origin, &["rev-parse", "hangar/task"]).trim(),
        second
    );
    assert!(push(&fx.wt, None).unwrap().up_to_date);

    // Someone else pushes to the same branch: our next push is not a fast-forward.
    let other = fx.root.join("other");
    git(
        &fx.root,
        &[
            "clone",
            "-q",
            "-b",
            "hangar/task",
            fx.origin.to_str().unwrap(),
            other.to_str().unwrap(),
        ],
    );
    configure(&other, &fx.hooks);
    fx.write(&other, "theirs.txt", "t\n");
    git(&other, &["add", "-A"]);
    git(&other, &["commit", "-q", "-m", "theirs"]);
    git(&other, &["push", "-q"]);
    let theirs = fx.head(&other);

    fx.commit_in_wt("c.txt", "c\n", "c");
    match push(&fx.wt, None) {
        Err(Error::PushRejected {
            non_fast_forward,
            reason,
            ..
        }) => {
            assert!(non_fast_forward, "{reason}");
        }
        other => panic!("expected PushRejected, got {other:?}"),
    }
    // Never forced.
    assert_eq!(
        git(&fx.origin, &["rev-parse", "hangar/task"]).trim(),
        theirs
    );

    // Pull and rebase, with an uncommitted edit carried over, then the push goes through.
    fx.write(&fx.wt, "wip.txt", "wip\n");
    let pulled = pull_rebase(&fx.wt).unwrap();
    assert_eq!(
        (pulled.upstream.as_str(), pulled.pulled),
        ("origin/hangar/task", 1)
    );
    assert!(fx.wt.join("theirs.txt").exists() && fx.wt.join("wip.txt").exists());
    assert!(!push(&fx.wt, None).unwrap().up_to_date);
    let log = git(&fx.origin, &["log", "--format=%s", "hangar/task"]);
    assert!(log.starts_with("c\ntheirs\n"), "{log}");
    assert_eq!(pull_rebase(&fx.wt).unwrap().pulled, 0);
}

#[test]
fn pull_rebase_conflict_is_aborted() {
    let fx = Fixture::new();
    fx.commit_in_wt("a.txt", "a\n", "a");
    push(&fx.wt, None).unwrap();
    let other = fx.root.join("other");
    git(
        &fx.root,
        &[
            "clone",
            "-q",
            "-b",
            "hangar/task",
            fx.origin.to_str().unwrap(),
            other.to_str().unwrap(),
        ],
    );
    configure(&other, &fx.hooks);
    fx.write(&other, "a.txt", "theirs\n");
    git(&other, &["commit", "-qam", "theirs"]);
    git(&other, &["push", "-q"]);
    let ours = fx.commit_in_wt("a.txt", "ours\n", "ours");

    match pull_rebase(&fx.wt) {
        Err(Error::RebaseConflicts { onto, paths }) => {
            assert_eq!(onto, "origin/hangar/task");
            assert_eq!(paths, [PathBuf::from("a.txt")]);
        }
        other => panic!("expected RebaseConflicts, got {other:?}"),
    }
    assert_eq!(fx.head(&fx.wt), ours);
    assert!(status(&fx.wt).unwrap().is_clean());

    git(&fx.wt, &["branch", "--unset-upstream"]);
    git(&fx.origin, &["branch", "-D", "hangar/task"]);
    assert!(matches!(pull_rebase(&fx.wt), Err(Error::NoUpstream(_))));
}

/// `other`, a clone of origin on `hangar/task` with one commit of `path` pushed.
fn push_from_other(fx: &Fixture, from: &str, path: &str, text: &str) {
    let other = fx.root.join("other");
    git(
        &fx.root,
        &[
            "clone",
            "-q",
            "-b",
            from,
            fx.origin.to_str().unwrap(),
            other.to_str().unwrap(),
        ],
    );
    configure(&other, &fx.hooks);
    git(&other, &["checkout", "-q", "-B", "hangar/task"]);
    fx.write(&other, path, text);
    git(&other, &["add", "-A"]);
    git(&other, &["commit", "-q", "-m", "theirs"]);
    git(&other, &["push", "-q", "origin", "hangar/task"]);
}

#[test]
fn pull_rebase_after_a_rejected_first_push_uses_the_same_named_branch() {
    let fx = Fixture::new();
    // The branch already exists on the remote (a deleted and recreated workspace).
    push_from_other(&fx, "main", "theirs.txt", "t\n");
    fx.commit_in_wt("a.txt", "a\n", "a");
    assert!(matches!(
        push(&fx.wt, None),
        Err(Error::PushRejected {
            non_fast_forward: true,
            ..
        })
    ));
    assert_eq!(upstream(&fx.wt).unwrap(), None);

    let pulled = pull_rebase(&fx.wt).unwrap();
    assert_eq!(
        (pulled.upstream.as_str(), pulled.pulled),
        ("origin/hangar/task", 1)
    );
    assert_eq!(
        upstream(&fx.wt).unwrap().unwrap().name,
        "origin/hangar/task"
    );
    assert!(fx.wt.join("theirs.txt").exists());
    assert!(!push(&fx.wt, None).unwrap().up_to_date);
}

#[test]
fn pull_rebase_reports_conflicts_left_by_uncommitted_changes() {
    let fx = Fixture::new();
    fx.commit_in_wt("a.txt", "a\n", "a");
    push(&fx.wt, None).unwrap();
    push_from_other(&fx, "hangar/task", "a.txt", "theirs\n");
    fx.write(&fx.wt, "a.txt", "mine, not committed\n");

    match pull_rebase(&fx.wt) {
        Err(Error::PulledWithConflicts { pulled, paths, .. }) => {
            assert_eq!(pulled, 1);
            assert_eq!(paths, [PathBuf::from("a.txt")]);
        }
        other => panic!("expected PulledWithConflicts, got {other:?}"),
    }
}

#[test]
fn pull_rebase_conflict_is_aborted_with_the_apply_backend() {
    let fx = Fixture::new();
    git(&fx.wt, &["config", "rebase.backend", "apply"]);
    fx.commit_in_wt("a.txt", "a\n", "a");
    push(&fx.wt, None).unwrap();
    push_from_other(&fx, "hangar/task", "a.txt", "theirs\n");
    let ours = fx.commit_in_wt("a.txt", "ours\n", "ours");

    assert!(matches!(
        pull_rebase(&fx.wt),
        Err(Error::RebaseConflicts { .. })
    ));
    assert_eq!(fx.head(&fx.wt), ours);
    assert!(status(&fx.wt).unwrap().is_clean());
}

#[test]
fn push_needs_a_branch_and_a_remote() {
    let fx = Fixture::new();
    git(&fx.repo, &["remote", "remove", "origin"]);
    fx.commit_in_wt("a.txt", "a\n", "a");
    assert!(matches!(push(&fx.wt, None), Err(Error::NoRemote)));
    git(&fx.wt, &["checkout", "-q", "--detach"]);
    assert!(matches!(push(&fx.wt, None), Err(Error::DetachedHead)));
}

#[test]
fn push_never_goes_to_a_tracked_base_branch() {
    let fx = Fixture::new();
    let main_before = git(&fx.origin, &["rev-parse", "main"]).trim().to_owned();

    // A branch started from `origin/main` by plain git tracks it.
    let tracked = fx.root.join("worktrees").join("tracked");
    git(
        &fx.repo,
        &[
            "worktree",
            "add",
            "-q",
            "--track",
            "-b",
            "hangar/tracked",
            tracked.to_str().unwrap(),
            "origin/main",
        ],
    );
    assert_eq!(
        git(&tracked, &["rev-parse", "--abbrev-ref", "@{u}"]).trim(),
        "origin/main"
    );
    assert_eq!(upstream(&tracked).unwrap(), None);
    fx.write(&tracked, "t.txt", "t\n");
    stage(&tracked, &[p("t.txt")]).unwrap();
    let head = commit(&tracked, "t", CommitOptions::default()).unwrap();

    let out = push(&tracked, None).unwrap();
    assert!(out.set_upstream);
    assert_eq!(git(&fx.origin, &["rev-parse", "main"]).trim(), main_before);
    assert_eq!(
        git(&fx.origin, &["rev-parse", "hangar/tracked"]).trim(),
        head
    );
    assert_eq!(
        upstream(&tracked).unwrap().unwrap().name,
        "origin/hangar/tracked"
    );

    // `create` from a remote-tracking base does not set up tracking at all.
    let wt = fx.root.join("worktrees").join("from-origin");
    create(
        &fx.repo,
        &CreateOptions {
            branch: "hangar/from-origin".into(),
            base: Some("origin/main".into()),
            path: wt.clone(),
            reuse_existing_branch: false,
        },
    )
    .unwrap();
    assert!(status(&wt).unwrap().upstream.is_none());
}
