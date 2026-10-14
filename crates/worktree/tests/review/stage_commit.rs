use super::*;

#[test]
fn stage_unstage_and_commit() {
    let fx = Fixture::new();
    fx.write(&fx.wt, "README.md", "one\nTWO\nthree\n");
    fx.write(&fx.wt, "new dir/new file.txt", "hello\n");
    fs::remove_file(fx.wt.join("lib.rs")).unwrap();

    stage(&fx.wt, &[p("new dir/new file.txt"), p("lib.rs")]).unwrap();
    let st = status(&fx.wt).unwrap();
    let staged: Vec<_> = st
        .changes
        .iter()
        .filter(|c| c.staged)
        .map(|c| c.path.clone())
        .collect();
    assert_eq!(staged, [p("lib.rs"), p("new dir/new file.txt")]);

    unstage(&fx.wt, &[p("lib.rs")]).unwrap();
    let st = status(&fx.wt).unwrap();
    assert_eq!(st.staged, 1);
    assert!(fx.wt.join("README.md").exists());

    // An empty list never means "everything".
    stage(&fx.wt, &[]).unwrap();
    unstage(&fx.wt, &[]).unwrap();
    assert_eq!(status(&fx.wt).unwrap().staged, 1);

    let before = fx.head(&fx.wt);
    let oid = commit(
        &fx.wt,
        "Add a file\n\nWith a body.",
        CommitOptions::default(),
    )
    .unwrap();
    assert_ne!(oid, before);
    assert_eq!(oid, fx.head(&fx.wt));
    assert_eq!(
        git(&fx.wt, &["log", "-1", "--format=%B"]).trim(),
        "Add a file\n\nWith a body."
    );
    // README edit and lib.rs deletion are still unstaged.
    let st = status(&fx.wt).unwrap();
    assert_eq!((st.staged, st.unstaged), (0, 2));

    let oid2 = commit(&fx.wt, "rest", CommitOptions { all: true }).unwrap();
    assert_ne!(oid2, oid);
    assert!(status(&fx.wt).unwrap().is_clean());

    fx.write(&fx.wt, "x.txt", "x");
    stage_all(&fx.wt).unwrap();
    assert_eq!(status(&fx.wt).unwrap().staged, 1);
}

#[test]
fn commit_refusals() {
    let fx = Fixture::new();
    assert!(matches!(
        commit(&fx.wt, "msg", CommitOptions::default()),
        Err(Error::NothingToCommit)
    ));
    fx.write(&fx.wt, "README.md", "changed\n");
    fx.write(&fx.wt, "untracked.txt", "u\n");
    // Unstaged only: nothing staged, and `all` ignores untracked files.
    assert!(matches!(
        commit(&fx.wt, "msg", CommitOptions::default()),
        Err(Error::NothingToCommit)
    ));
    stage(&fx.wt, &[p("README.md")]).unwrap();
    assert!(matches!(
        commit(&fx.wt, "  \n ", CommitOptions::default()),
        Err(Error::EmptyMessage)
    ));

    let head = fx.head(&fx.wt);
    fx.hook("pre-commit", "echo 'lint failed: bad style' >&2; exit 1");
    match commit(&fx.wt, "msg", CommitOptions::default()) {
        Err(Error::HookFailed { stderr }) => assert!(stderr.contains("lint failed"), "{stderr}"),
        other => panic!("expected HookFailed, got {other:?}"),
    }
    assert_eq!(fx.head(&fx.wt), head);
    assert_eq!(status(&fx.wt).unwrap().staged, 1);

    fs::remove_file(fx.hooks.join("pre-commit")).unwrap();
    commit(&fx.wt, "msg", CommitOptions::default()).unwrap();
}
