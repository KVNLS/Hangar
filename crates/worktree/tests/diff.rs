//! Unified diff, ignore checks and git dirs against real temporary repositories.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use hangar_worktree::{
    DiffOptions, FileDiff, FileStatus, LineKind, check_ignored, diff_file, diff_files,
    diff_files_with, diff_paths, git_dirs, list_files, merge_base,
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

struct Repo {
    _tmp: TempDir,
    path: PathBuf,
}

impl Repo {
    /// `main` with one commit, then a `feature` branch checked out.
    fn new() -> Self {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().canonicalize().unwrap().join("repo");
        fs::create_dir(&path).unwrap();
        git(&path, &["init", "-q", "-b", "main"]);
        git(&path, &["config", "user.name", "Hangar Test"]);
        git(&path, &["config", "user.email", "test@hangar.invalid"]);
        git(&path, &["config", "commit.gpgsign", "false"]);
        git(&path, &["config", "core.autocrlf", "false"]);
        let repo = Self { _tmp: tmp, path };
        repo.write("README.md", "one\ntwo\nthree\n");
        repo.write("lib.rs", "fn a() {}\n");
        repo.write(
            "long.txt",
            &(1..=20).map(|i| format!("line {i}\n")).collect::<String>(),
        );
        repo.write(".gitignore", "*.log\nignored/\n");
        commit_all(&repo.path, "initial");
        git(&repo.path, &["checkout", "-q", "-b", "feature"]);
        repo
    }

    fn write(&self, rel: &str, content: &str) {
        self.write_bytes(rel, content.as_bytes());
    }

    fn write_bytes(&self, rel: &str, content: &[u8]) {
        let path = self.path.join(rel);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        fs::write(path, content).unwrap();
    }

    fn diff(&self) -> Vec<FileDiff> {
        diff_files(&self.path, "main").unwrap()
    }
}

fn find<'a>(files: &'a [FileDiff], path: &str) -> &'a FileDiff {
    files
        .iter()
        .find(|f| f.path == Path::new(path))
        .unwrap_or_else(|| panic!("{path} missing from {files:#?}"))
}

fn texts(file: &FileDiff) -> Vec<(LineKind, String)> {
    file.hunks
        .iter()
        .flat_map(|h| h.lines.iter().map(|l| (l.kind, l.text.clone())))
        .collect()
}

#[test]
fn clean_worktree_has_no_diff() {
    let repo = Repo::new();
    assert!(repo.diff().is_empty());
}

#[test]
fn modify_add_delete() {
    let repo = Repo::new();
    repo.write("README.md", "one\nTWO\nthree\n");
    repo.write("staged.rs", "new\n");
    git(&repo.path, &["add", "staged.rs"]);
    fs::remove_file(repo.path.join("lib.rs")).unwrap();

    let files = repo.diff();
    assert_eq!(files.len(), 3, "{files:#?}");
    let readme = find(&files, "README.md");
    assert_eq!(readme.status, FileStatus::Modified);
    assert_eq!((readme.insertions, readme.deletions), (1, 1));
    assert_eq!(
        texts(readme),
        vec![
            (LineKind::Context, "one".into()),
            (LineKind::Removed, "two".into()),
            (LineKind::Added, "TWO".into()),
            (LineKind::Context, "three".into()),
        ]
    );
    let staged = find(&files, "staged.rs");
    assert_eq!(staged.status, FileStatus::Added);
    assert_eq!(staged.new_mode, Some(0o100644));
    let lib = find(&files, "lib.rs");
    assert_eq!(lib.status, FileStatus::Deleted);
    assert_eq!(texts(lib), vec![(LineKind::Removed, "fn a() {}".into())]);
}

#[test]
fn includes_commits_since_merge_base_but_not_base_progress() {
    let repo = Repo::new();
    repo.write("feature.rs", "feature\n");
    commit_all(&repo.path, "feature work");
    git(&repo.path, &["checkout", "-q", "main"]);
    repo.write("main_only.rs", "main\n");
    commit_all(&repo.path, "main moves on");
    git(&repo.path, &["checkout", "-q", "feature"]);

    let files = repo.diff();
    assert_eq!(files.len(), 1, "{files:#?}");
    assert_eq!(files[0].path, Path::new("feature.rs"));
    assert_eq!(files[0].status, FileStatus::Added);
    let mb = merge_base(&repo.path, "main").unwrap();
    assert_eq!(mb.len(), 40);
}

#[test]
fn detects_renames() {
    let repo = Repo::new();
    fs::create_dir(repo.path.join("moved")).unwrap();
    git(&repo.path, &["mv", "long.txt", "moved/long text.txt"]);
    repo.write(
        "moved/long text.txt",
        &(1..=20)
            .map(|i| match i {
                5 => "line five\n".to_owned(),
                _ => format!("line {i}\n"),
            })
            .collect::<String>(),
    );
    let files = repo.diff();
    assert_eq!(files.len(), 1, "{files:#?}");
    let moved = &files[0];
    assert_eq!(moved.status, FileStatus::Renamed);
    assert_eq!(moved.path, Path::new("moved/long text.txt"));
    assert_eq!(moved.old_path.as_deref(), Some(Path::new("long.txt")));
    assert_eq!((moved.insertions, moved.deletions), (1, 1));
}

#[test]
fn list_files_tracked_and_untracked_not_ignored_or_deleted() {
    let repo = Repo::new();
    repo.write("src/new.rs", "fn n() {}\n");
    repo.write("debug.log", "ignored\n");
    repo.write("ignored/inside.txt", "ignored\n");
    fs::remove_file(repo.path.join("long.txt")).unwrap();
    let files = list_files(&repo.path).unwrap();
    assert_eq!(
        files,
        [".gitignore", "README.md", "lib.rs", "src/new.rs"].map(PathBuf::from)
    );
}

#[test]
fn untracked_files_with_odd_names() {
    let repo = Repo::new();
    repo.write("dir with space/naïve file.txt", "a\nb");
    repo.write("tab\there.txt", "t\n");
    repo.write("quote\"d.txt", "q\n");
    repo.write("empty.txt", "");
    repo.write("debug.log", "ignored\n");
    repo.write("ignored/inside.txt", "ignored\n");

    let files = repo.diff();
    let paths: Vec<_> = files.iter().map(|f| f.path.clone()).collect();
    assert_eq!(
        paths,
        vec![
            PathBuf::from("dir with space/naïve file.txt"),
            PathBuf::from("empty.txt"),
            PathBuf::from("quote\"d.txt"),
            PathBuf::from("tab\there.txt"),
        ]
    );
    let naive = &files[0];
    assert_eq!(naive.status, FileStatus::Untracked);
    assert_eq!(
        texts(naive),
        vec![
            (LineKind::Added, "a".into()),
            (LineKind::Added, "b".into()),
            (LineKind::NoNewlineAtEof, "No newline at end of file".into()),
        ]
    );
    let empty = &files[1];
    assert_eq!(empty.status, FileStatus::Untracked);
    assert!(empty.hunks.is_empty() && !empty.binary);
}

#[test]
fn binary_crlf_mode_and_no_newline() {
    let repo = Repo::new();
    repo.write_bytes("bin.dat", b"\x00\x01\x02");
    repo.write_bytes("crlf.txt", b"a\r\nb\r\n");
    repo.write("nonl.txt", "x");
    commit_all(&repo.path, "more files");
    git(&repo.path, &["branch", "-f", "main"]);

    repo.write_bytes("bin.dat", b"\x00\x09\x02");
    repo.write_bytes("crlf.txt", b"a\r\nB\r\n");
    repo.write("nonl.txt", "y");
    repo.write_bytes("new.bin", b"\x00untracked");
    let script = repo.path.join("lib.rs");
    let mut perms = fs::metadata(&script).unwrap().permissions();
    std::os::unix::fs::PermissionsExt::set_mode(&mut perms, 0o755);
    fs::set_permissions(&script, perms).unwrap();

    let files = repo.diff();
    assert_eq!(files.len(), 5, "{files:#?}");
    let bin = find(&files, "bin.dat");
    assert!(bin.binary && bin.hunks.is_empty());
    let new_bin = find(&files, "new.bin");
    assert!(new_bin.binary);
    assert_eq!(new_bin.status, FileStatus::Untracked);
    assert_eq!(
        texts(find(&files, "crlf.txt")),
        vec![
            (LineKind::Context, "a".into()),
            (LineKind::Removed, "b".into()),
            (LineKind::Added, "B".into()),
        ]
    );
    let nonl = texts(find(&files, "nonl.txt"));
    assert_eq!(nonl.len(), 4);
    assert_eq!(nonl[1].0, LineKind::NoNewlineAtEof);
    let lib = find(&files, "lib.rs");
    assert_eq!(
        (lib.old_mode, lib.new_mode),
        (Some(0o100644), Some(0o100755))
    );
    assert!(lib.hunks.is_empty());
}

#[test]
fn type_change_to_symlink() {
    let repo = Repo::new();
    fs::remove_file(repo.path.join("lib.rs")).unwrap();
    std::os::unix::fs::symlink("README.md", repo.path.join("lib.rs")).unwrap();
    let files = repo.diff();
    assert_eq!(files.len(), 1, "{files:#?}");
    assert_eq!(files[0].status, FileStatus::TypeChanged);
    assert_eq!(files[0].new_mode, Some(0o120000));
    assert_eq!((files[0].insertions, files[0].deletions), (1, 1));
}

#[test]
fn single_file_and_path_limited_diffs() {
    let repo = Repo::new();
    repo.write("README.md", "changed\n");
    repo.write("src/a.rs", "a\n");
    repo.write("src/b.rs", "b\n");

    let readme = diff_file(&repo.path, "main", Path::new("README.md"))
        .unwrap()
        .unwrap();
    assert_eq!(readme.status, FileStatus::Modified);
    assert!(
        diff_file(&repo.path, "main", Path::new("lib.rs"))
            .unwrap()
            .is_none()
    );
    let untracked = diff_file(&repo.path, "main", Path::new("src/a.rs"))
        .unwrap()
        .unwrap();
    assert_eq!(untracked.status, FileStatus::Untracked);

    let rev = merge_base(&repo.path, "main").unwrap();
    let under_src = diff_paths(
        &repo.path,
        &rev,
        &[PathBuf::from("src")],
        &DiffOptions::default(),
    )
    .unwrap();
    assert_eq!(under_src.len(), 2);
    // Literal pathspecs: `*` must not glob.
    let star = diff_paths(
        &repo.path,
        &rev,
        &[PathBuf::from("*")],
        &DiffOptions::default(),
    )
    .unwrap();
    assert!(star.is_empty());
}

#[test]
fn large_file_guards() {
    let repo = Repo::new();
    repo.write(
        "long.txt",
        &(1..=20)
            .map(|i| format!("changed {i}\n"))
            .collect::<String>(),
    );
    repo.write("huge.txt", &"x\n".repeat(1000));
    let opts = DiffOptions {
        max_lines_per_file: Some(5),
        max_untracked_bytes: Some(100),
        detect_renames: true,
    };
    let files = diff_files_with(&repo.path, "main", &opts).unwrap();
    let long = find(&files, "long.txt");
    assert!(long.truncated);
    assert_eq!(long.line_count(), 5);
    assert_eq!((long.insertions, long.deletions), (20, 20));
    let huge = find(&files, "huge.txt");
    assert!(huge.truncated && huge.hunks.is_empty());
    assert_eq!(huge.status, FileStatus::Untracked);
}

#[test]
fn plain_mv_shows_as_a_rename() {
    let repo = Repo::new();
    fs::create_dir(repo.path.join("moved")).unwrap();
    fs::rename(repo.path.join("long.txt"), repo.path.join("moved/long.txt")).unwrap();
    repo.write("fresh.txt", "new\n");
    let files = repo.diff();
    assert_eq!(files.len(), 2, "{files:#?}");
    let moved = find(&files, "moved/long.txt");
    assert_eq!(moved.status, FileStatus::Renamed);
    assert_eq!(moved.old_path.as_deref(), Some(Path::new("long.txt")));
    let fresh = find(&files, "fresh.txt");
    assert_eq!(fresh.status, FileStatus::Untracked);
    assert_eq!(texts(fresh), [(LineKind::Added, "new".to_owned())]);
    let pid = format!("hangar-diff-index-{}-", std::process::id());
    let leftovers: Vec<_> = fs::read_dir(std::env::temp_dir())
        .unwrap()
        .flatten()
        .filter(|e| e.file_name().to_string_lossy().starts_with(&pid))
        .collect();
    assert!(
        leftovers.is_empty(),
        "scratch index left behind: {leftovers:?}"
    );
}

#[test]
fn diff_does_not_touch_the_index() {
    let repo = Repo::new();
    repo.write("README.md", "changed\n");
    repo.write("untracked.txt", "u\n");
    let index = repo.path.join(".git/index");
    let before = fs::read(&index).unwrap();
    let modified = fs::metadata(&index).unwrap().modified().unwrap();
    repo.diff();
    assert_eq!(fs::read(&index).unwrap(), before);
    assert_eq!(fs::metadata(&index).unwrap().modified().unwrap(), modified);
    assert!(git(&repo.path, &["diff", "--cached", "--name-only"]).is_empty());
}

#[test]
fn check_ignored_filters_paths() {
    let repo = Repo::new();
    repo.write("ignored/real.txt", "x\n");
    let paths = [
        "a.log",
        "ignored/real.txt",
        "deep/missing.log",
        "README.md",
        "src/new.rs",
        "with space.log",
    ]
    .map(PathBuf::from);
    let ignored = check_ignored(&repo.path, &paths).unwrap();
    assert_eq!(
        ignored,
        [
            "a.log",
            "ignored/real.txt",
            "deep/missing.log",
            "with space.log"
        ]
        .map(PathBuf::from)
    );
    assert!(check_ignored(&repo.path, &[]).unwrap().is_empty());
    assert!(
        check_ignored(&repo.path, &[PathBuf::from("README.md")])
            .unwrap()
            .is_empty()
    );
}

#[test]
fn git_dirs_of_linked_worktree() {
    let repo = Repo::new();
    let linked = repo.path.parent().unwrap().join("linked");
    git(
        &repo.path,
        &[
            "worktree",
            "add",
            "-q",
            "-b",
            "wt",
            linked.to_str().unwrap(),
        ],
    );
    let dirs = git_dirs(&linked).unwrap();
    assert_eq!(dirs.common_dir, repo.path.join(".git"));
    assert_eq!(dirs.git_dir, repo.path.join(".git/worktrees/linked"));
    let main = git_dirs(&repo.path).unwrap();
    assert_eq!(main.git_dir, main.common_dir);
}

#[test]
fn partial_status_merges_into_full() {
    let repo = Repo::new();
    repo.write("README.md", "changed\n");
    repo.write("src/a.rs", "a\n");
    let mut full = hangar_worktree::status(&repo.path).unwrap();
    assert_eq!((full.unstaged, full.untracked), (1, 1));

    repo.write("src/b.rs", "b\n");
    fs::remove_file(repo.path.join("src/a.rs")).unwrap();
    git(&repo.path, &["checkout", "--", "README.md"]);
    let paths = [PathBuf::from("src")];
    let partial = hangar_worktree::status_paths(&repo.path, &paths).unwrap();
    assert_eq!(partial.changes.len(), 1);
    assert_eq!(partial.branch.as_deref(), Some("feature"));
    full.merge_partial(partial, &paths);
    // README.md is outside the paths, so its stale entry stays until it is refreshed too.
    let names: Vec<_> = full.changes.iter().map(|c| c.path.clone()).collect();
    assert_eq!(names, ["README.md", "src/b.rs"].map(PathBuf::from));
    assert_eq!((full.unstaged, full.untracked), (1, 1));

    let paths = [PathBuf::from("README.md")];
    full.merge_partial(
        hangar_worktree::status_paths(&repo.path, &paths).unwrap(),
        &paths,
    );
    assert_eq!(full, hangar_worktree::status(&repo.path).unwrap());
}

#[test]
fn merge_conflict_is_reported() {
    let repo = Repo::new();
    repo.write("README.md", "feature\n");
    commit_all(&repo.path, "feature");
    git(&repo.path, &["checkout", "-q", "-b", "other", "main"]);
    repo.write("README.md", "other\n");
    commit_all(&repo.path, "other");
    git(&repo.path, &["checkout", "-q", "feature"]);
    let merge = Command::new("git")
        .arg("-C")
        .arg(&repo.path)
        .args(["merge", "-q", "--no-edit", "other"])
        .output()
        .unwrap();
    assert!(!merge.status.success());

    let files = repo.diff();
    assert_eq!(files.len(), 1, "{files:#?}");
    assert_eq!(files[0].path, Path::new("README.md"));
    // `git diff <commit>` shows the conflicted worktree content; `status` flags the conflict.
    assert_eq!(files[0].status, FileStatus::Modified);
    assert!(
        texts(&files[0])
            .iter()
            .any(|(kind, text)| *kind == LineKind::Added && text.starts_with("<<<<<<<"))
    );
}
