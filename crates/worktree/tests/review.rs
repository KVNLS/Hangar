//! Stage, commit, push, merge and the `gh` wrapper against temporary repositories with a local
//! bare `origin` and a fake `gh` script. Nothing here talks to a real remote.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;

use hangar_worktree::{
    CommitOptions, CreateOptions, CreatePr, Error, Gh, GhStatus, MergeStrategy, PrInfo,
    base_checkout, commit, create, merge_into_base, pull_rebase, push, remotes, stage, stage_all,
    status, unstage, upstream,
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

fn configure(dir: &Path, hooks: &Path) {
    git(dir, &["config", "user.name", "Hangar Test"]);
    git(dir, &["config", "user.email", "test@hangar.invalid"]);
    git(dir, &["config", "commit.gpgsign", "false"]);
    // Isolate from any global hooks directory.
    git(dir, &["config", "core.hooksPath", hooks.to_str().unwrap()]);
}

/// `repo` on `main` (pushed to a bare `origin`) plus a linked worktree on `hangar/task`.
struct Fixture {
    _tmp: TempDir,
    root: PathBuf,
    repo: PathBuf,
    origin: PathBuf,
    wt: PathBuf,
    hooks: PathBuf,
}

impl Fixture {
    fn new() -> Self {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path().canonicalize().unwrap();
        let hooks = root.join("hooks");
        fs::create_dir(&hooks).unwrap();
        let origin = root.join("origin.git");
        git(
            &root,
            &[
                "init",
                "-q",
                "--bare",
                "-b",
                "main",
                origin.to_str().unwrap(),
            ],
        );
        configure(&origin, &hooks);
        let repo = root.join("my repo");
        fs::create_dir(&repo).unwrap();
        git(&repo, &["init", "-q", "-b", "main"]);
        configure(&repo, &hooks);
        fs::write(repo.join("README.md"), "one\ntwo\nthree\n").unwrap();
        fs::write(repo.join("lib.rs"), "fn a() {}\n").unwrap();
        git(&repo, &["add", "-A"]);
        git(&repo, &["commit", "-q", "-m", "initial"]);
        git(
            &repo,
            &["remote", "add", "origin", origin.to_str().unwrap()],
        );
        git(&repo, &["push", "-q", "-u", "origin", "main"]);
        let wt = root.join("worktrees").join("task");
        create(
            &repo,
            &CreateOptions {
                branch: "hangar/task".into(),
                base: Some("main".into()),
                path: wt.clone(),
                reuse_existing_branch: false,
            },
        )
        .unwrap();
        Self {
            _tmp: tmp,
            root,
            repo,
            origin,
            wt,
            hooks,
        }
    }

    fn write(&self, dir: &Path, path: &str, text: &str) {
        let full = dir.join(path);
        fs::create_dir_all(full.parent().unwrap()).unwrap();
        fs::write(full, text).unwrap();
    }

    fn hook(&self, name: &str, script: &str) {
        let path = self.hooks.join(name);
        fs::write(&path, format!("#!/bin/sh\n{script}\n")).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
    }

    fn head(&self, dir: &Path) -> String {
        git(dir, &["rev-parse", "HEAD"]).trim().to_owned()
    }

    /// Commits `path` on the workspace branch through the API.
    fn commit_in_wt(&self, path: &str, text: &str, message: &str) -> String {
        self.write(&self.wt, path, text);
        stage(&self.wt, &[PathBuf::from(path)]).unwrap();
        commit(&self.wt, message, CommitOptions::default()).unwrap()
    }
}

fn p(s: &str) -> PathBuf {
    PathBuf::from(s)
}

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
    assert!(matches!(pull_rebase(&fx.wt), Err(Error::NoUpstream(_))));
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

/// A fake `gh` that records its arguments and the env it got, one line each, then `--`.
struct FakeGh {
    _tmp: TempDir,
    dir: PathBuf,
    log: PathBuf,
}

impl FakeGh {
    fn new(logged_in: bool) -> Self {
        let tmp = TempDir::new().unwrap();
        let dir = tmp.path().canonicalize().unwrap();
        let log = dir.join("args.log");
        let auth = if logged_in {
            "echo 'github.com'; echo '  ✓ Logged in to github.com account fake-user (keyring)'; echo '  - Token: gho_************'; exit 0"
        } else {
            "echo 'You are not logged into any GitHub hosts. To log in, run: gh auth login' >&2; exit 1"
        };
        let script = format!(
            r#"#!/bin/sh
for a in "$@"; do printf '%s\n' "$a" >> '{log}'; done
echo "env GH_PROMPT_DISABLED=$GH_PROMPT_DISABLED NO_COLOR=$NO_COLOR" >> '{log}'
echo "cwd $(pwd -P)" >> '{log}'
echo -- >> '{log}'
case "$1 $2" in
  "auth status") {auth} ;;
  "pr view")
    if [ "$3" = "hangar/none" ]; then echo "no pull requests found for branch \"$3\"" >&2; exit 1; fi
    if [ "$3" = "hangar/broken" ]; then echo "HTTP 502" >&2; exit 1; fi
    echo '{{"number":7,"state":"OPEN","title":"Fix login","url":"https://github.com/o/r/pull/7"}}' ;;
  "pr create") echo 'Creating pull request for hangar/task into main in o/r'; echo; echo 'https://github.com/o/r/pull/8' ;;
  *) echo "unexpected" >&2; exit 2 ;;
esac
"#,
            log = log.display()
        );
        let gh = dir.join("gh");
        fs::write(&gh, script).unwrap();
        fs::set_permissions(&gh, fs::Permissions::from_mode(0o755)).unwrap();
        Self {
            _tmp: tmp,
            dir,
            log,
        }
    }

    fn gh(&self) -> Gh {
        Gh::new().with_path_env(format!("{}:/usr/bin:/bin", self.dir.display()))
    }

    fn calls(&self) -> Vec<Vec<String>> {
        let text = fs::read_to_string(&self.log).unwrap_or_default();
        text.split("--\n")
            .filter(|c| !c.is_empty())
            .map(|c| c.lines().map(str::to_owned).collect())
            .collect()
    }
}

#[test]
fn gh_status_view_and_create() {
    let fake = FakeGh::new(true);
    let gh = fake.gh();
    assert_eq!(
        gh.status().unwrap(),
        GhStatus::Installed {
            authenticated: true,
            account: Some("fake-user".into())
        }
    );
    let fx = Fixture::new();
    assert_eq!(
        gh.pr_for_branch(&fx.wt, "hangar/task").unwrap(),
        Some(PrInfo {
            number: 7,
            url: "https://github.com/o/r/pull/7".into(),
            state: "OPEN".into(),
            title: "Fix login".into(),
        })
    );
    assert_eq!(gh.pr_for_branch(&fx.wt, "hangar/none").unwrap(), None);
    assert!(matches!(
        gh.pr_for_branch(&fx.wt, "hangar/broken"),
        Err(Error::Gh { .. })
    ));
    let url = gh
        .create_pr(
            &fx.wt,
            &CreatePr {
                base: "main".into(),
                head: "hangar/task".into(),
                title: "Fix login".into(),
                body: "Line 1\nLine 2".into(),
                draft: true,
            },
        )
        .unwrap();
    assert_eq!(url, "https://github.com/o/r/pull/8");

    let calls = fake.calls();
    assert_eq!(calls.len(), 5);
    assert_eq!(calls[0][..2], ["auth", "status"]);
    assert!(
        calls
            .iter()
            .all(|c| c.contains(&"env GH_PROMPT_DISABLED=1 NO_COLOR=1".to_owned()))
    );
    assert_eq!(
        calls[1][..5],
        [
            "pr",
            "view",
            "hangar/task",
            "--json",
            "number,url,state,title"
        ]
    );
    assert!(calls[1].contains(&format!("cwd {}", fx.wt.display())));
    assert_eq!(
        calls[4][..12],
        [
            "pr",
            "create",
            "--base",
            "main",
            "--head",
            "hangar/task",
            "--title",
            "Fix login",
            "--body",
            "Line 1",
            "Line 2",
            "--draft"
        ]
    );
}

#[test]
fn gh_unauthenticated_and_missing() {
    let fake = FakeGh::new(false);
    assert_eq!(
        fake.gh().status().unwrap(),
        GhStatus::Installed {
            authenticated: false,
            account: None
        }
    );
    let empty = TempDir::new().unwrap();
    let gh = Gh::new().with_path_env(empty.path());
    assert_eq!(gh.status().unwrap(), GhStatus::Missing);
    assert!(matches!(
        gh.pr_for_branch(empty.path(), "x"),
        Err(Error::GhMissing)
    ));
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
