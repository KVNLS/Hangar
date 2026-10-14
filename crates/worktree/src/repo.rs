use std::path::{Path, PathBuf};

use crate::error::Result;
use crate::git::Git;

/// Top-level directory of the working tree containing `path` (a linked worktree's own root).
pub fn repo_root(path: &Path) -> Result<PathBuf> {
    let out = Git::new(path).run(["rev-parse", "--show-toplevel"])?;
    Ok(PathBuf::from(out.trim_end_matches(['\n', '\r'])))
}

/// Checked-out branch, or `None` when HEAD is detached.
pub fn current_branch(path: &Path) -> Result<Option<String>> {
    let out = Git::new(path).query(["symbolic-ref", "--quiet", "--short", "HEAD"])?;
    Ok(out.map(|s| s.trim().to_owned()))
}

pub fn branch_exists(repo: &Path, branch: &str) -> Result<bool> {
    let out = Git::new(repo).query([
        "rev-parse",
        "--verify",
        "--quiet",
        &format!("refs/heads/{branch}"),
    ])?;
    Ok(out.is_some())
}

/// Best guess at the branch new work should start from, as a revision usable as a base:
/// `origin/HEAD`'s target (e.g. `origin/main`), else local `main`, else `master`, else the
/// current branch. `None` if HEAD is detached and none of those exist.
pub fn default_branch(repo: &Path) -> Result<Option<String>> {
    let origin_head = Git::new(repo).query([
        "symbolic-ref",
        "--quiet",
        "--short",
        "refs/remotes/origin/HEAD",
    ])?;
    if let Some(target) = origin_head.as_deref().map(str::trim)
        && !target.is_empty()
    {
        return Ok(Some(target.to_owned()));
    }
    for candidate in ["main", "master"] {
        if branch_exists(repo, candidate)? {
            return Ok(Some(candidate.to_owned()));
        }
    }
    current_branch(repo)
}
