use std::path::{Path, PathBuf};

use crate::error::{Error, Result};
use crate::git::{Git, bytes_to_path};

/// The subset of `paths` (relative to the worktree, existing or not) that git ignores.
/// Tracked files are never reported, matching `git check-ignore` without `--no-index`.
pub fn check_ignored(worktree: &Path, paths: &[PathBuf]) -> Result<Vec<PathBuf>> {
    if paths.is_empty() {
        return Ok(Vec::new());
    }
    let mut input = Vec::new();
    for path in paths {
        input.extend_from_slice(path.as_os_str().as_encoded_bytes());
        input.push(0);
    }
    let out =
        Git::new(worktree).run_bytes_with(["check-ignore", "--stdin", "-z"], Some(&input), &[1])?;
    Ok(out
        .split(|&b| b == 0)
        .filter(|f| !f.is_empty())
        .map(bytes_to_path)
        .collect())
}

/// Absolute git directories of a worktree. For a linked worktree `git_dir` is
/// `<repo>/.git/worktrees/<name>` (HEAD, index) and `common_dir` is `<repo>/.git` (refs).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GitDirs {
    pub git_dir: PathBuf,
    pub common_dir: PathBuf,
}

pub fn git_dirs(worktree: &Path) -> Result<GitDirs> {
    let out = Git::new(worktree).run([
        "rev-parse",
        "--path-format=absolute",
        "--git-dir",
        "--git-common-dir",
    ])?;
    let mut lines = out.lines();
    match (lines.next(), lines.next()) {
        (Some(git_dir), Some(common_dir)) => Ok(GitDirs {
            git_dir: PathBuf::from(git_dir),
            common_dir: PathBuf::from(common_dir),
        }),
        _ => Err(Error::Parse(format!("rev-parse git dirs: {out:?}"))),
    }
}
