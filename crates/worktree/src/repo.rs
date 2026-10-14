use std::path::{Path, PathBuf};

use hangar_core::ports::BranchTip;

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

/// Local branches with their tip's short hash and committer date, sorted by name.
pub fn branch_tips(repo: &Path) -> Result<Vec<BranchTip>> {
    let out = Git::new(repo).run([
        "for-each-ref",
        "--format=%(refname:short)%00%(objectname:short)%00%(committerdate:unix)",
        "refs/heads",
    ])?;
    Ok(parse_branch_tips(&out))
}

fn parse_branch_tips(out: &str) -> Vec<BranchTip> {
    let mut tips: Vec<BranchTip> = out
        .lines()
        .filter_map(|line| {
            let mut fields = line.split('\x00');
            let name = fields.next()?.trim();
            let short_sha = fields.next()?.trim();
            let committed_at = fields.next()?.trim().parse().ok()?;
            (!name.is_empty()).then(|| BranchTip {
                name: name.to_owned(),
                short_sha: short_sha.to_owned(),
                committed_at,
            })
        })
        .collect();
    tips.sort_by(|a, b| a.name.cmp(&b.name));
    tips
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

#[cfg(test)]
mod tests {
    use super::parse_branch_tips;

    #[test]
    fn parses_branch_tips() {
        let tips = parse_branch_tips(
            "main\x00abc1234\x001700000000\nfeat/x\x00def5678\x001700000100\nbad\x00x\n",
        );
        let names: Vec<&str> = tips.iter().map(|t| t.name.as_str()).collect();
        assert_eq!(names, ["feat/x", "main"]);
        assert_eq!(tips[1].short_sha, "abc1234");
        assert_eq!(tips[1].committed_at, 1_700_000_000);
    }
}
