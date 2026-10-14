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

/// Local branch names, sorted.
pub fn local_branches(repo: &Path) -> Result<Vec<String>> {
    let out = Git::new(repo).run(["for-each-ref", "--format=%(refname:short)", "refs/heads"])?;
    let mut branches: Vec<String> = out
        .lines()
        .map(str::trim)
        .filter(|b| !b.is_empty())
        .map(str::to_owned)
        .collect();
    branches.sort();
    Ok(branches)
}

/// Whether `name` can be a new branch's name (`git check-ref-format --branch`).
pub fn is_valid_branch_name(repo: &Path, name: &str) -> Result<bool> {
    if name.is_empty() || name.starts_with('-') {
        return Ok(false);
    }
    // Exits 128 for an invalid name.
    let out = Git::new(repo).output(["check-ref-format", "--branch", name], None)?;
    Ok(out.status.success())
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

/// The branch a workspace base names, without its remote (`origin/main` → `main`), or `None`
/// when the base is a commit, a tag, `HEAD` (which moves) or does not resolve.
pub fn base_branch_name(repo: &Path, base: &str) -> Result<Option<String>> {
    let base = base.trim();
    if base.is_empty() || base == "HEAD" || base.starts_with('-') {
        return Ok(None);
    }
    let out = Git::new(repo).query([
        "rev-parse",
        "--verify",
        "--quiet",
        "--symbolic-full-name",
        base,
    ])?;
    Ok(out.as_deref().and_then(branch_of_ref))
}

/// `refs/heads/x` → `x`, `refs/remotes/origin/x` → `x`; anything else → `None`.
fn branch_of_ref(full: &str) -> Option<String> {
    let full = full.trim();
    let name = match full.strip_prefix("refs/heads/") {
        Some(local) => local,
        None => full.strip_prefix("refs/remotes/")?.split_once('/')?.1,
    };
    (!name.is_empty() && name != "HEAD").then(|| name.to_owned())
}
