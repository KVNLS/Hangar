//! Watcher and LiveDiff against real temporary repositories and FSEvents.
//! Timing: positive waits are generous; negative checks use a sentinel write instead of
//! "nothing arrived within N ms", so slow machines cannot make them pass or fail by luck.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;
use std::time::Duration;

use hangar_files::{ChangeBatch, DiffUpdate, LiveDiff, LiveDiffConfig, Watcher, WatcherConfig};
use hangar_worktree::{FileStatus, GitCli, LineKind};
use tempfile::TempDir;
use tokio::sync::mpsc::UnboundedReceiver;
use tokio::time::timeout;

const WAIT: Duration = Duration::from_secs(10);

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

struct Repo {
    _tmp: TempDir,
    path: PathBuf,
}

impl Repo {
    fn new() -> Self {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().canonicalize().unwrap().join("repo");
        fs::create_dir(&path).unwrap();
        git(&path, &["init", "-q", "-b", "main"]);
        git(&path, &["config", "user.name", "Hangar Test"]);
        git(&path, &["config", "user.email", "test@hangar.invalid"]);
        git(&path, &["config", "commit.gpgsign", "false"]);
        fs::write(path.join("README.md"), "one\ntwo\nthree\n").unwrap();
        fs::write(path.join(".gitignore"), "*.log\ndist/\n").unwrap();
        git(&path, &["add", "-A"]);
        git(&path, &["commit", "-q", "--no-verify", "-m", "initial"]);
        Self { _tmp: tmp, path }
    }

    fn write(&self, rel: &str, content: &str) {
        let path = self.path.join(rel);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, content).unwrap();
    }
}

fn config() -> WatcherConfig {
    WatcherConfig {
        debounce: Duration::from_millis(100),
        ..WatcherConfig::default()
    }
}

/// Lets FSEvents settle so events from the repository setup are not mistaken for test events.
async fn settle<T>(rx: &mut UnboundedReceiver<T>) {
    while let Ok(Some(_)) = timeout(Duration::from_millis(400), rx.recv()).await {}
}

/// Collects batches until one contains `sentinel`.
async fn batches_until(
    rx: &mut UnboundedReceiver<ChangeBatch>,
    sentinel: &str,
) -> Vec<ChangeBatch> {
    let mut seen = Vec::new();
    loop {
        let batch = timeout(WAIT, rx.recv())
            .await
            .expect("timed out waiting for sentinel")
            .expect("watcher stopped");
        let done = batch.paths.contains(Path::new(sentinel));
        seen.push(batch);
        if done {
            return seen;
        }
    }
}

#[tokio::test]
async fn write_produces_batch() {
    let repo = Repo::new();
    let (watcher, mut rx) = Watcher::start(Arc::new(GitCli), &repo.path, config()).unwrap();
    assert_eq!(watcher.root(), repo.path);
    settle(&mut rx).await;
    repo.write("src/new file.rs", "fn main() {}\n");
    let batches = batches_until(&mut rx, "src/new file.rs").await;
    assert!(batches.iter().all(|b| !b.rescan));
}

#[tokio::test]
async fn ignored_and_heavy_paths_produce_nothing() {
    let repo = Repo::new();
    let (_watcher, mut rx) = Watcher::start(Arc::new(GitCli), &repo.path, config()).unwrap();
    settle(&mut rx).await;
    repo.write("debug.log", "x\n");
    repo.write("dist/bundle.js", "x\n");
    repo.write("node_modules/pkg/index.js", "x\n");
    repo.write("ios/Pods/Lib/a.h", "x\n");
    tokio::time::sleep(Duration::from_millis(500)).await;
    repo.write("sentinel.txt", "x\n");
    for batch in batches_until(&mut rx, "sentinel.txt").await {
        assert_eq!(
            batch.paths.iter().collect::<Vec<_>>(),
            vec![Path::new("sentinel.txt")],
            "{batch:?}"
        );
    }
}

#[tokio::test]
async fn commit_sets_git_meta_changed() {
    let repo = Repo::new();
    repo.write("a.txt", "a\n");
    let (_watcher, mut rx) = Watcher::start(Arc::new(GitCli), &repo.path, config()).unwrap();
    settle(&mut rx).await;
    git(&repo.path, &["add", "a.txt"]);
    git(&repo.path, &["commit", "-q", "--no-verify", "-m", "a"]);
    loop {
        let batch = timeout(WAIT, rx.recv()).await.unwrap().unwrap();
        if batch.git_meta_changed {
            break;
        }
    }
}

#[tokio::test]
async fn linked_worktree_commit_sets_git_meta_changed() {
    let repo = Repo::new();
    let linked = repo.path.parent().unwrap().join("linked wt");
    git(
        &repo.path,
        &[
            "worktree",
            "add",
            "-q",
            "-b",
            "task",
            linked.to_str().unwrap(),
        ],
    );
    let (_watcher, mut rx) = Watcher::start(Arc::new(GitCli), &linked, config()).unwrap();
    settle(&mut rx).await;
    fs::write(linked.join("b.txt"), "b\n").unwrap();
    git(&linked, &["add", "b.txt"]);
    git(&linked, &["commit", "-q", "--no-verify", "-m", "b"]);
    let mut meta = false;
    let mut file = false;
    while !(meta && file) {
        let batch = timeout(WAIT, rx.recv()).await.unwrap().unwrap();
        meta |= batch.git_meta_changed;
        file |= batch.paths.contains(Path::new("b.txt"));
    }
}

#[tokio::test]
async fn reading_git_state_does_not_trigger_meta_changes() {
    let repo = Repo::new();
    repo.write("README.md", "changed\n");
    repo.write("new.txt", "n\n");
    let (_watcher, mut rx) = Watcher::start(Arc::new(GitCli), &repo.path, config()).unwrap();
    settle(&mut rx).await;
    for _ in 0..3 {
        hangar_worktree::status(&repo.path).unwrap();
        hangar_worktree::diff_files(&repo.path, "main").unwrap();
    }
    tokio::time::sleep(Duration::from_millis(300)).await;
    repo.write("sentinel.txt", "x\n");
    for batch in batches_until(&mut rx, "sentinel.txt").await {
        assert!(!batch.git_meta_changed, "{batch:?}");
    }
}

#[tokio::test]
async fn dropping_the_watcher_closes_the_channel() {
    let repo = Repo::new();
    let (watcher, mut rx) = Watcher::start(Arc::new(GitCli), &repo.path, config()).unwrap();
    drop(watcher);
    while timeout(WAIT, rx.recv()).await.unwrap().is_some() {}
}

async fn next_update(rx: &mut UnboundedReceiver<hangar_files::Result<DiffUpdate>>) -> DiffUpdate {
    timeout(WAIT, rx.recv())
        .await
        .expect("timed out waiting for a diff update")
        .expect("live diff stopped")
        .expect("live diff error")
}

/// Waits for an update satisfying `pred`, skipping intermediate ones.
async fn update_where(
    rx: &mut UnboundedReceiver<hangar_files::Result<DiffUpdate>>,
    pred: impl Fn(&DiffUpdate) -> bool,
) -> DiffUpdate {
    loop {
        let update = next_update(rx).await;
        if pred(&update) {
            return update;
        }
    }
}

fn live_config() -> LiveDiffConfig {
    LiveDiffConfig {
        watcher: config(),
        ..LiveDiffConfig::default()
    }
}

#[tokio::test]
async fn live_diff_tracks_edit_and_checkout() {
    let repo = Repo::new();
    git(&repo.path, &["checkout", "-q", "-b", "feature"]);
    let (live, mut rx) =
        LiveDiff::start(Arc::new(GitCli), &repo.path, "main", live_config()).unwrap();

    let initial = next_update(&mut rx).await;
    assert!(initial.full);
    assert!(initial.changed.is_empty() && initial.removed.is_empty());
    assert_eq!(initial.summary.files, 0);
    assert_eq!(initial.summary.status.branch.as_deref(), Some("feature"));

    repo.write("README.md", "one\nTWO\nthree\n");
    let edited = update_where(&mut rx, |u| {
        u.changed.iter().any(|f| f.path == Path::new("README.md"))
    })
    .await;
    assert!(edited.trigger.is_some());
    let readme = edited
        .changed
        .iter()
        .find(|f| f.path == Path::new("README.md"))
        .unwrap();
    assert_eq!(readme.status, FileStatus::Modified);
    let lines: Vec<_> = readme.hunks[0]
        .lines
        .iter()
        .map(|l| (l.kind, l.text.as_str()))
        .collect();
    assert_eq!(
        lines,
        vec![
            (LineKind::Context, "one"),
            (LineKind::Removed, "two"),
            (LineKind::Added, "TWO"),
            (LineKind::Context, "three"),
        ]
    );
    assert_eq!(
        (edited.summary.insertions, edited.summary.deletions),
        (1, 1)
    );
    // The incrementally patched status matches a full one.
    assert_eq!(
        edited.summary.status,
        hangar_worktree::status(&repo.path).unwrap()
    );
    assert_eq!(live.file(Path::new("README.md")).as_ref(), Some(readme));

    git(&repo.path, &["checkout", "--", "README.md"]);
    let reverted = update_where(&mut rx, |u| {
        u.removed.iter().any(|p| p == Path::new("README.md"))
    })
    .await;
    assert_eq!(reverted.summary.files, 0);
    assert!(live.snapshot().is_empty());
}

#[tokio::test]
async fn live_diff_untracked_ignored_and_commit() {
    let repo = Repo::new();
    git(&repo.path, &["checkout", "-q", "-b", "feature"]);
    let (live, mut rx) =
        LiveDiff::start(Arc::new(GitCli), &repo.path, "main", live_config()).unwrap();
    next_update(&mut rx).await;

    repo.write("debug.log", "ignored\n");
    repo.write("src/new.rs", "fn new() {}\n");
    let added = update_where(&mut rx, |u| {
        u.changed.iter().any(|f| f.path == Path::new("src/new.rs"))
    })
    .await;
    assert!(
        added
            .changed
            .iter()
            .all(|f| f.path != Path::new("debug.log"))
    );
    assert_eq!(
        live.file(Path::new("src/new.rs")).unwrap().status,
        FileStatus::Untracked
    );

    // Committed work stays in the diff (it is relative to the merge-base) but changes status.
    git(&repo.path, &["add", "src/new.rs"]);
    git(&repo.path, &["commit", "-q", "--no-verify", "-m", "new"]);
    let committed = update_where(&mut rx, |u| {
        u.changed
            .iter()
            .any(|f| f.path == Path::new("src/new.rs") && f.status == FileStatus::Added)
    })
    .await;
    assert!(committed.full);
    assert_eq!(committed.summary.status.untracked, 0);
    assert_eq!(live.snapshot().len(), 1);

    // Deleting the file again removes it from the diff.
    git(&repo.path, &["rm", "-q", "src/new.rs"]);
    git(&repo.path, &["commit", "-q", "--no-verify", "-m", "drop"]);
    update_where(&mut rx, |u| {
        u.removed.iter().any(|p| p == Path::new("src/new.rs"))
    })
    .await;
    assert!(live.snapshot().is_empty());
}

#[tokio::test]
async fn live_diff_directory_rename() {
    let repo = Repo::new();
    repo.write("old dir/a.rs", "a\n");
    repo.write("old dir/sub/b.rs", "b\n");
    git(&repo.path, &["add", "-A"]);
    git(&repo.path, &["commit", "-q", "--no-verify", "-m", "dir"]);
    git(&repo.path, &["checkout", "-q", "-b", "feature"]);
    let (live, mut rx) =
        LiveDiff::start(Arc::new(GitCli), &repo.path, "main", live_config()).unwrap();
    next_update(&mut rx).await;

    // Moved without `git mv`: each file pairs with its deleted original as a rename.
    fs::rename(repo.path.join("old dir"), repo.path.join("new dir")).unwrap();
    update_where(&mut rx, |_| {
        let snapshot = live.snapshot();
        snapshot.len() == 2 && snapshot.contains_key(Path::new("new dir/sub/b.rs"))
    })
    .await;
    let snapshot = live.snapshot();
    let moved = &snapshot[Path::new("new dir/sub/b.rs")];
    assert_eq!(moved.status, FileStatus::Renamed);
    assert_eq!(
        moved.old_path.as_deref(),
        Some(Path::new("old dir/sub/b.rs"))
    );
    assert_eq!(
        snapshot[Path::new("new dir/a.rs")].status,
        FileStatus::Renamed
    );

    fs::rename(repo.path.join("new dir"), repo.path.join("old dir")).unwrap();
    update_where(&mut rx, |_| live.snapshot().is_empty()).await;
}

#[tokio::test]
async fn live_diff_refresh_and_bad_base() {
    let repo = Repo::new();
    let (live, mut rx) = LiveDiff::start(
        Arc::new(GitCli),
        &repo.path,
        "no-such-branch",
        live_config(),
    )
    .unwrap();
    let first = timeout(WAIT, rx.recv()).await.unwrap().unwrap();
    assert!(first.is_err());
    live.set_base("main").unwrap();
    let update = next_update(&mut rx).await;
    assert!(update.full);
    assert_eq!(update.summary.files, 0);
    assert!(live.summary().is_some());
    live.refresh().unwrap();
}

#[tokio::test]
async fn ignoring_files_removes_them_from_the_live_diff() {
    let repo = Repo::new();
    git(&repo.path, &["checkout", "-q", "-b", "feature"]);
    let (live, mut rx) =
        LiveDiff::start(Arc::new(GitCli), &repo.path, "main", live_config()).unwrap();
    next_update(&mut rx).await;

    repo.write("gen/out.bin", "generated\n");
    update_where(&mut rx, |u| {
        u.changed.iter().any(|f| f.path == Path::new("gen/out.bin"))
    })
    .await;

    // Only `.gitignore` changes, yet the now-ignored file must leave the diff.
    let ignore = fs::read_to_string(repo.path.join(".gitignore")).unwrap_or_default();
    repo.write(".gitignore", &format!("{ignore}gen/\n"));
    update_where(&mut rx, |u| {
        u.removed.iter().any(|p| p == Path::new("gen/out.bin"))
    })
    .await;
    assert!(live.file(Path::new("gen/out.bin")).is_none());
}
