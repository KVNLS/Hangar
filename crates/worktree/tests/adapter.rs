//! `GitCli` through the `VersionControl` port, against a real temporary repository.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::fs;
use std::path::Path;
use std::process::Command;

use hangar_core::ports::{ChangeCounts, Changes, NewWorktree, VersionControl};
use hangar_worktree::GitCli;
use tempfile::TempDir;

fn git(dir: &Path, args: &[&str]) {
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
}

#[test]
fn worktree_lifecycle_through_the_port() {
    let tmp = TempDir::new().unwrap();
    let root = tmp.path().canonicalize().unwrap();
    let repo = root.join("repo");
    fs::create_dir(&repo).unwrap();
    git(&repo, &["init", "-q", "-b", "main"]);
    git(&repo, &["config", "user.name", "Hangar Test"]);
    git(&repo, &["config", "user.email", "test@hangar.invalid"]);
    git(&repo, &["config", "commit.gpgsign", "false"]);
    fs::write(repo.join("README.md"), "one\n").unwrap();
    git(&repo, &["add", "-A"]);
    git(&repo, &["commit", "--no-verify", "-q", "-m", "initial"]);

    let vc: &dyn VersionControl = &GitCli;
    assert_eq!(vc.repo_root(&repo.join(".")).unwrap(), repo);
    assert_eq!(vc.default_branch(&repo).unwrap().as_deref(), Some("main"));
    assert!(vc.is_valid_branch_name(&repo, "hangar/x").unwrap());
    assert!(!vc.is_valid_branch_name(&repo, "bad name").unwrap());
    assert!(!vc.branch_exists(&repo, "hangar/x").unwrap());

    let path = root.join("worktrees/x");
    let created = vc
        .add_worktree(
            &repo,
            &NewWorktree {
                branch: "hangar/x".into(),
                base: "main".into(),
                path: path.clone(),
            },
        )
        .unwrap();
    assert_eq!(created, path);
    assert!(vc.branch_exists(&repo, "hangar/x").unwrap());
    assert_eq!(vc.local_branches(&repo).unwrap(), ["hangar/x", "main"]);
    let tips = vc.branch_tips(&repo).unwrap();
    let names: Vec<&str> = tips.iter().map(|t| t.name.as_str()).collect();
    assert_eq!(names, ["hangar/x", "main"]);
    assert_eq!(tips[0].short_sha, tips[1].short_sha);
    assert!(tips[0].short_sha.len() >= 7 && tips[0].committed_at > 0);
    let listed = vc.worktrees(&repo).unwrap();
    assert_eq!(listed.len(), 2);
    assert!(listed[0].is_main && !listed[1].is_main);
    assert_eq!(listed[1].path, path);

    assert_eq!(vc.change_counts(&path).unwrap(), ChangeCounts::default());
    assert_eq!(vc.commits_ahead(&path, "main").unwrap(), 0);
    fs::write(path.join("README.md"), "two\n").unwrap();
    fs::write(path.join("new.txt"), "new\n").unwrap();
    let mut files = Changes::list_files(&GitCli, &path).unwrap();
    files.sort();
    assert_eq!(files, [Path::new("README.md"), Path::new("new.txt")]);
    assert_eq!(
        vc.change_counts(&path).unwrap(),
        ChangeCounts {
            uncommitted: 1,
            untracked: 1,
            submodules: 0,
        }
    );
    git(&path, &["add", "-A"]);
    git(&path, &["commit", "--no-verify", "-q", "-m", "work"]);
    assert_eq!(vc.commits_ahead(&path, "main").unwrap(), 1);

    vc.remove_worktree(&repo, &path, false).unwrap();
    vc.prune_worktrees(&repo).unwrap();
    assert_eq!(vc.worktrees(&repo).unwrap().len(), 1);
    // Unmerged: refused without force.
    let err = vc.delete_branch(&repo, "hangar/x", false).unwrap_err();
    assert!(!err.to_string().is_empty());
    vc.delete_branch(&repo, "hangar/x", true).unwrap();
    assert!(!vc.branch_exists(&repo, "hangar/x").unwrap());
}

#[test]
fn initialized_submodules_are_counted_and_need_force_to_remove() {
    let tmp = TempDir::new().unwrap();
    let root = tmp.path().canonicalize().unwrap();
    let init = |dir: &Path| {
        fs::create_dir(dir).unwrap();
        git(dir, &["init", "-q", "-b", "main"]);
        git(dir, &["config", "user.name", "Hangar Test"]);
        git(dir, &["config", "user.email", "test@hangar.invalid"]);
        git(dir, &["config", "commit.gpgsign", "false"]);
        fs::write(dir.join("README.md"), "one\n").unwrap();
        git(dir, &["add", "-A"]);
        git(dir, &["commit", "--no-verify", "-q", "-m", "initial"]);
    };
    let (lib, repo) = (root.join("lib"), root.join("repo"));
    init(&lib);
    init(&repo);
    let file = ["-c", "protocol.file.allow=always"];
    git(
        &repo,
        &[
            file[0],
            file[1],
            "submodule",
            "add",
            "-q",
            lib.to_str().unwrap(),
            "lib",
        ],
    );
    git(&repo, &["commit", "--no-verify", "-q", "-m", "submodule"]);

    let vc: &dyn VersionControl = &GitCli;
    let path = root.join("worktrees/x");
    vc.add_worktree(
        &repo,
        &NewWorktree {
            branch: "hangar/x".into(),
            base: "main".into(),
            path: path.clone(),
        },
    )
    .unwrap();
    // Not initialized yet in the new worktree.
    assert_eq!(vc.change_counts(&path).unwrap().submodules, 0);
    git(
        &path,
        &[file[0], file[1], "submodule", "update", "-q", "--init"],
    );
    let counts = vc.change_counts(&path).unwrap();
    assert_eq!(
        (counts.uncommitted, counts.untracked, counts.submodules),
        (0, 0, 1)
    );

    assert!(vc.remove_worktree(&repo, &path, false).is_err());
    vc.remove_worktree(&repo, &path, true).unwrap();
}
