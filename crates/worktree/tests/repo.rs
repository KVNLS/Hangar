//! Integration tests against real temporary repositories.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use hangar_worktree::{
    ChangeKind, CreateOptions, Error, base_branch_name, create, current_branch, default_branch,
    delete_branch, diff_stat, is_valid_branch_name, list, local_branches, prune, remove, repo_root,
    status,
};
use tempfile::TempDir;

fn git(dir: &Path, args: &[&str]) -> String {
    let out = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .env("LC_ALL", "C")
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8(out.stdout).unwrap()
}

fn commit_all(dir: &Path, message: &str) {
    git(dir, &["add", "-A"]);
    git(dir, &["commit", "--no-verify", "-q", "-m", message]);
}

/// A repository on `main` with one commit, inside a directory whose name contains a space.
struct Fixture {
    _tmp: TempDir,
    root: PathBuf,
    repo: PathBuf,
}

impl Fixture {
    fn new() -> Self {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path().canonicalize().unwrap();
        let repo = root.join("my repo");
        fs::create_dir(&repo).unwrap();
        git(&repo, &["init", "-q", "-b", "main"]);
        git(&repo, &["config", "user.name", "Hangar Test"]);
        git(&repo, &["config", "user.email", "test@hangar.invalid"]);
        git(&repo, &["config", "commit.gpgsign", "false"]);
        fs::write(repo.join("README.md"), "one\ntwo\nthree\n").unwrap();
        fs::write(repo.join("lib.rs"), "fn a() {}\n").unwrap();
        commit_all(&repo, "initial");
        Self {
            _tmp: tmp,
            root,
            repo,
        }
    }

    fn worktree(&self, branch: &str, dir: &str) -> PathBuf {
        let path = self.root.join("worktrees").join(dir);
        create(
            &self.repo,
            &CreateOptions {
                branch: branch.into(),
                base: Some("main".into()),
                path: path.clone(),
                reuse_existing_branch: false,
            },
        )
        .unwrap();
        path
    }
}

#[test]
fn repo_queries() {
    let fx = Fixture::new();
    assert_eq!(repo_root(&fx.repo).unwrap(), fx.repo);
    fs::create_dir(fx.repo.join("sub")).unwrap();
    assert_eq!(repo_root(&fx.repo.join("sub")).unwrap(), fx.repo);
    assert_eq!(current_branch(&fx.repo).unwrap().as_deref(), Some("main"));
    assert_eq!(default_branch(&fx.repo).unwrap().as_deref(), Some("main"));

    git(&fx.repo, &["checkout", "-q", "--detach"]);
    assert_eq!(current_branch(&fx.repo).unwrap(), None);
    assert_eq!(default_branch(&fx.repo).unwrap().as_deref(), Some("main"));

    let head = git(&fx.repo, &["rev-parse", "HEAD"]);
    git(
        &fx.repo,
        &["update-ref", "refs/remotes/origin/trunk", head.trim()],
    );
    git(
        &fx.repo,
        &[
            "symbolic-ref",
            "refs/remotes/origin/HEAD",
            "refs/remotes/origin/trunk",
        ],
    );
    assert_eq!(
        default_branch(&fx.repo).unwrap().as_deref(),
        Some("origin/trunk")
    );

    git(&fx.repo, &["branch", "feature/x"]);
    let branches = local_branches(&fx.repo).unwrap();
    assert!(branches.contains(&"main".to_owned()) && branches.contains(&"feature/x".to_owned()));
    assert!(!branches.iter().any(|b| b.starts_with("origin/")));
    assert!(is_valid_branch_name(&fx.repo, "fix/login-2").unwrap());
    for bad in ["", "-x", "a..b", "has space", "end.lock", "a~b"] {
        assert!(!is_valid_branch_name(&fx.repo, bad).unwrap(), "{bad}");
    }
}

#[test]
fn base_branch_names() {
    let fx = Fixture::new();
    let wt = fx.worktree("hangar/x", "x");
    let head = git(&fx.repo, &["rev-parse", "HEAD"]);
    let head = head.trim();
    git(
        &fx.repo,
        &["update-ref", "refs/remotes/origin/feature/y", head],
    );
    git(&fx.repo, &["tag", "v1"]);
    let name = |base: &str| base_branch_name(&wt, base).unwrap();
    assert_eq!(name("main").as_deref(), Some("main"));
    assert_eq!(name("origin/feature/y").as_deref(), Some("feature/y"));
    assert_eq!(name(head), None, "a commit");
    assert_eq!(name(&head[..7]), None, "a short commit");
    assert_eq!(name("v1"), None, "a tag");
    assert_eq!(name("HEAD"), None, "moves with the worktree");
    assert_eq!(name("gone"), None);
    assert_eq!(name(""), None);
    assert_eq!(name("--all"), None);
}

#[test]
fn create_two_worktrees_and_list() {
    let fx = Fixture::new();
    let a = fx.worktree("task/a", "task a");
    let b = fx.worktree("task-b", "b");

    let wts = list(&fx.repo).unwrap();
    assert_eq!(wts.len(), 3);
    assert!(wts[0].is_main);
    assert_eq!(wts[0].path, fx.repo);
    assert_eq!(wts[0].branch.as_deref(), Some("main"));
    let wa = wts.iter().find(|w| w.path == a).unwrap();
    assert_eq!(wa.branch.as_deref(), Some("task/a"));
    assert!(!wa.is_main && !wa.locked && !wa.prunable);
    assert_eq!(wa.head, git(&fx.repo, &["rev-parse", "main"]).trim());
    assert!(wts.iter().any(|w| w.path == b));
    assert_eq!(current_branch(&a).unwrap().as_deref(), Some("task/a"));
    assert_eq!(repo_root(&a).unwrap(), a);

    git(&fx.repo, &["worktree", "lock", b.to_str().unwrap()]);
    git(&a, &["checkout", "-q", "--detach"]);
    let wts = list(&fx.repo).unwrap();
    assert!(wts.iter().find(|w| w.path == b).unwrap().locked);
    assert_eq!(wts.iter().find(|w| w.path == a).unwrap().branch, None);
}

#[test]
fn create_returns_worktree_and_respects_existing_branch() {
    let fx = Fixture::new();
    git(&fx.repo, &["branch", "existing"]);
    let path = fx.root.join("wt existing");
    let options = CreateOptions {
        branch: "existing".into(),
        base: None,
        path: path.clone(),
        reuse_existing_branch: false,
    };
    match create(&fx.repo, &options) {
        Err(Error::BranchExists(branch)) => assert_eq!(branch, "existing"),
        other => panic!("expected BranchExists, got {other:?}"),
    }
    assert!(!path.exists());

    let wt = create(
        &fx.repo,
        &CreateOptions {
            reuse_existing_branch: true,
            ..options
        },
    )
    .unwrap();
    assert_eq!(wt.path, path);
    assert_eq!(wt.branch.as_deref(), Some("existing"));
    assert!(!wt.is_main);

    // A branch checked out elsewhere cannot be reused; git's own error comes through.
    let err = create(
        &fx.repo,
        &CreateOptions {
            branch: "existing".into(),
            base: None,
            path: fx.root.join("again"),
            reuse_existing_branch: true,
        },
    )
    .unwrap_err();
    assert!(matches!(err, Error::Git { .. }), "{err:?}");
}

#[test]
fn status_counts_and_ahead_behind() {
    let fx = Fixture::new();
    git(&fx.repo, &["branch", "upstream"]);
    let wt = fx.worktree("feature", "feature wt");

    let clean = status(&wt).unwrap();
    assert!(clean.is_clean());
    assert_eq!(clean.branch.as_deref(), Some("feature"));
    assert_eq!(clean.upstream, None);

    git(&wt, &["branch", "--set-upstream-to=upstream"]);
    fs::write(wt.join("feature.rs"), "fn f() {}\n").unwrap();
    commit_all(&wt, "feature commit");
    git(&fx.repo, &["checkout", "-q", "upstream"]);
    fs::write(fx.repo.join("upstream.rs"), "fn u() {}\n").unwrap();
    commit_all(&fx.repo, "upstream commit 1");
    fs::write(fx.repo.join("upstream2.rs"), "fn v() {}\n").unwrap();
    commit_all(&fx.repo, "upstream commit 2");

    fs::write(wt.join("README.md"), "one\nTWO\nthree\n").unwrap();
    git(&wt, &["add", "README.md"]);
    fs::write(wt.join("lib.rs"), "fn a() { }\n").unwrap();
    fs::write(wt.join("feature.rs"), "fn f() { }\n").unwrap();
    git(&wt, &["add", "feature.rs"]);
    fs::write(wt.join("feature.rs"), "fn f() {  }\n").unwrap();
    fs::create_dir(wt.join("new dir")).unwrap();
    fs::write(wt.join("new dir/untracked file.txt"), "x").unwrap();
    git(&wt, &["mv", "lib.rs", "moved.rs"]);

    let s = status(&wt).unwrap();
    assert_eq!(s.branch.as_deref(), Some("feature"));
    assert_eq!(s.upstream.as_deref(), Some("upstream"));
    assert_eq!((s.ahead, s.behind), (1, 2));
    assert_eq!(
        s.oid.as_deref(),
        Some(git(&wt, &["rev-parse", "HEAD"]).trim())
    );
    // Staged: README.md, feature.rs, rename. Unstaged: feature.rs, moved.rs (edited before mv).
    assert_eq!(
        (s.staged, s.unstaged, s.untracked, s.conflicted),
        (3, 2, 1, 0)
    );
    let renamed = s
        .changes
        .iter()
        .find(|c| c.kind == ChangeKind::Renamed)
        .unwrap();
    assert_eq!(renamed.path, PathBuf::from("moved.rs"));
    assert_eq!(renamed.orig_path, Some(PathBuf::from("lib.rs")));
    let untracked = s
        .changes
        .iter()
        .find(|c| c.kind == ChangeKind::Untracked)
        .unwrap();
    assert_eq!(untracked.path, PathBuf::from("new dir/untracked file.txt"));
}

#[test]
fn status_reports_conflicts() {
    let fx = Fixture::new();
    git(&fx.repo, &["checkout", "-q", "-b", "other"]);
    fs::write(fx.repo.join("README.md"), "other\n").unwrap();
    commit_all(&fx.repo, "other");
    git(&fx.repo, &["checkout", "-q", "main"]);
    fs::write(fx.repo.join("README.md"), "main\n").unwrap();
    commit_all(&fx.repo, "main");
    let merge = Command::new("git")
        .arg("-C")
        .arg(&fx.repo)
        .args(["merge", "-q", "--no-edit", "other"])
        .output()
        .unwrap();
    assert!(!merge.status.success());

    let s = status(&fx.repo).unwrap();
    assert_eq!(s.conflicted, 1);
    assert_eq!(s.changes[0].kind, ChangeKind::Conflicted);
    assert_eq!(s.changes[0].path, PathBuf::from("README.md"));
}

#[test]
fn diff_stat_includes_commits_and_working_tree() {
    let fx = Fixture::new();
    let wt = fx.worktree("diffs", "diff wt");
    fs::write(fx.repo.join("main-only.rs"), "x\n").unwrap();
    commit_all(&fx.repo, "advance main");

    fs::write(wt.join("committed file.rs"), "a\nb\n").unwrap();
    commit_all(&wt, "add file");
    fs::write(wt.join("README.md"), "one\nthree\nfour\nfive\n").unwrap();
    fs::write(wt.join("lib.rs"), "fn a() {}\nfn b() {}\n").unwrap();
    git(&wt, &["add", "lib.rs"]);
    fs::write(wt.join("bin.dat"), [0u8, 1, 2, 0, 255]).unwrap();
    git(&wt, &["add", "bin.dat"]);
    fs::write(wt.join("untracked.rs"), "u\n").unwrap();

    let mut stats = diff_stat(&wt, "main").unwrap();
    stats.sort_by(|a, b| a.path.cmp(&b.path));
    let summary: Vec<_> = stats
        .iter()
        .map(|s| (s.path.to_str().unwrap(), s.insertions, s.deletions))
        .collect();
    assert_eq!(
        summary,
        vec![
            ("README.md", Some(2), Some(1)),
            ("bin.dat", None, None),
            ("committed file.rs", Some(2), Some(0)),
            ("lib.rs", Some(1), Some(0)),
        ]
    );
    assert!(stats[1].insertions.is_none());

    assert!(matches!(
        diff_stat(&wt, "no-such-branch"),
        Err(Error::Git { .. })
    ));
}

#[test]
fn remove_prune_and_delete_branch() {
    let fx = Fixture::new();
    let keep = fx.worktree("keep", "keep");
    let dirty = fx.worktree("dirty", "dirty wt");
    let gone = fx.worktree("gone", "gone");

    remove(&fx.repo, &keep, false).unwrap();
    assert!(!keep.exists());

    fs::write(dirty.join("README.md"), "changed\n").unwrap();
    assert!(matches!(
        remove(&fx.repo, &dirty, false),
        Err(Error::Git { .. })
    ));
    remove(&fx.repo, &dirty, true).unwrap();
    assert!(!dirty.exists());

    fs::remove_dir_all(&gone).unwrap();
    let wts = list(&fx.repo).unwrap();
    assert_eq!(wts.len(), 2);
    assert!(wts.iter().find(|w| w.path == gone).unwrap().prunable);
    prune(&fx.repo).unwrap();
    assert_eq!(list(&fx.repo).unwrap().len(), 1);

    delete_branch(&fx.repo, "keep", false).unwrap();
    fs::write(fx.repo.join("x.rs"), "x").unwrap();
    git(&fx.repo, &["checkout", "-q", "-b", "unmerged"]);
    commit_all(&fx.repo, "unmerged");
    git(&fx.repo, &["checkout", "-q", "main"]);
    assert!(matches!(
        delete_branch(&fx.repo, "unmerged", false),
        Err(Error::Git { .. })
    ));
    delete_branch(&fx.repo, "unmerged", true).unwrap();
    let branches = git(&fx.repo, &["branch", "--format=%(refname:short)"]);
    assert!(!branches.lines().any(|b| b == "keep" || b == "unmerged"));
}

#[test]
fn not_a_repo_is_explicit() {
    let tmp = TempDir::new().unwrap();
    let dir = tmp.path().join("plain dir");
    fs::create_dir(&dir).unwrap();
    for result in [
        repo_root(&dir).map(|_| ()),
        list(&dir).map(|_| ()),
        status(&dir).map(|_| ()),
        current_branch(&dir).map(|_| ()),
    ] {
        match result {
            Err(Error::NotARepo(path)) => assert_eq!(path, dir),
            other => panic!("expected NotARepo, got {other:?}"),
        }
    }
}
