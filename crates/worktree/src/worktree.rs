use std::ffi::OsString;
use std::path::{Path, PathBuf};

use crate::error::{Error, Result};
use crate::git::{Git, bytes_to_path, bytes_to_string};
use crate::repo::branch_exists;

/// One entry of `git worktree list`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Worktree {
    pub path: PathBuf,
    /// Short branch name; `None` when detached (or bare).
    pub branch: Option<String>,
    /// Commit checked out; empty for a bare repository.
    pub head: String,
    pub bare: bool,
    pub locked: bool,
    pub prunable: bool,
    /// The repository's main working tree (always listed first by git).
    pub is_main: bool,
}

#[derive(Clone, Debug)]
pub struct CreateOptions {
    pub branch: String,
    /// Start point of a new branch; `None` means the repository's HEAD. Ignored when reusing.
    pub base: Option<String>,
    /// Chosen by the caller; git creates missing parent directories.
    pub path: PathBuf,
    /// Check out `branch` if it already exists instead of failing with [`Error::BranchExists`].
    pub reuse_existing_branch: bool,
}

/// Adds a linked worktree at `options.path` on a new (or reused) branch.
pub fn create(repo: &Path, options: &CreateOptions) -> Result<Worktree> {
    let git = Git::new(repo);
    let mut args: Vec<OsString> = vec!["worktree".into(), "add".into()];
    if branch_exists(repo, &options.branch)? {
        if !options.reuse_existing_branch {
            return Err(Error::BranchExists(options.branch.clone()));
        }
        args.extend([
            "--".into(),
            options.path.clone().into(),
            options.branch.clone().into(),
        ]);
    } else {
        args.extend([
            "-b".into(),
            options.branch.clone().into(),
            "--".into(),
            options.path.clone().into(),
            options.base.clone().unwrap_or_else(|| "HEAD".into()).into(),
        ]);
    }
    git.run_bytes(&args)?;

    let wanted = options.path.canonicalize()?;
    list(repo)?
        .into_iter()
        .find(|wt| wt.path.canonicalize().is_ok_and(|p| p == wanted))
        .ok_or_else(|| {
            Error::Parse(format!(
                "new worktree {} missing from `git worktree list`",
                options.path.display()
            ))
        })
}

pub fn list(repo: &Path) -> Result<Vec<Worktree>> {
    let out = Git::new(repo).run_bytes(["worktree", "list", "--porcelain", "-z"])?;
    parse_list(&out)
}

/// Removes a linked worktree; `force` discards uncommitted changes and untracked files.
pub fn remove(repo: &Path, path: &Path, force: bool) -> Result<()> {
    let mut args: Vec<OsString> = vec!["worktree".into(), "remove".into()];
    if force {
        args.push("--force".into());
    }
    args.extend(["--".into(), path.as_os_str().to_owned()]);
    Git::new(repo).run_bytes(&args)?;
    Ok(())
}

/// Drops administrative data for worktrees whose directories no longer exist.
pub fn prune(repo: &Path) -> Result<()> {
    Git::new(repo).run_bytes(["worktree", "prune"])?;
    Ok(())
}

/// `git branch -d` (or `-D` with `force`, which also deletes unmerged branches).
pub fn delete_branch(repo: &Path, branch: &str, force: bool) -> Result<()> {
    let flag = if force { "-D" } else { "-d" };
    Git::new(repo).run_bytes(["branch", flag, "--", branch])?;
    Ok(())
}

/// Parses `git worktree list --porcelain -z`: NUL-terminated attributes, records separated by
/// an empty attribute.
pub fn parse_list(out: &[u8]) -> Result<Vec<Worktree>> {
    let mut worktrees = Vec::new();
    let mut current: Option<Worktree> = None;
    for field in out.split(|&b| b == 0) {
        if field.is_empty() {
            worktrees.extend(current.take());
            continue;
        }
        let (key, value) = match field.iter().position(|&b| b == b' ') {
            Some(i) => (&field[..i], &field[i + 1..]),
            None => (field, &[][..]),
        };
        if key == b"worktree" {
            worktrees.extend(current.take());
            current = Some(Worktree {
                path: bytes_to_path(value),
                branch: None,
                head: String::new(),
                bare: false,
                locked: false,
                prunable: false,
                is_main: worktrees.is_empty(),
            });
            continue;
        }
        let Some(wt) = current.as_mut() else {
            return Err(Error::Parse(format!(
                "worktree attribute before `worktree`: {:?}",
                bytes_to_string(field)
            )));
        };
        match key {
            b"HEAD" => wt.head = bytes_to_string(value),
            b"branch" => {
                let name = bytes_to_string(value);
                let short = name.strip_prefix("refs/heads/").unwrap_or(&name).to_owned();
                wt.branch = Some(short);
            }
            b"bare" => wt.bare = true,
            b"locked" => wt.locked = true,
            b"prunable" => wt.prunable = true,
            // `detached` is implied by a missing branch; ignore attributes added by newer git.
            _ => {}
        }
    }
    worktrees.extend(current);
    Ok(worktrees)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_porcelain_list() {
        let out = b"worktree /repo\0HEAD 1111\0branch refs/heads/main\0\0\
worktree /wt/with space\0HEAD 2222\0detached\0locked reason here\0\0\
worktree /wt/gone\0HEAD 3333\0branch refs/heads/feature/x\0prunable gitdir file points to non-existent location\0\0";
        let list = parse_list(out).unwrap();
        assert_eq!(list.len(), 3);
        assert_eq!(
            list[0],
            Worktree {
                path: "/repo".into(),
                branch: Some("main".into()),
                head: "1111".into(),
                bare: false,
                locked: false,
                prunable: false,
                is_main: true,
            }
        );
        assert_eq!(list[1].path, PathBuf::from("/wt/with space"));
        assert_eq!(list[1].branch, None);
        assert!(list[1].locked && !list[1].prunable && !list[1].is_main);
        assert_eq!(list[2].branch.as_deref(), Some("feature/x"));
        assert!(list[2].prunable && !list[2].locked);
    }

    #[test]
    fn parses_bare_and_bare_locked() {
        let out =
            b"worktree /bare.git\0bare\0\0worktree /wt\0HEAD abcd\0branch refs/heads/b\0locked\0\0";
        let list = parse_list(out).unwrap();
        assert!(list[0].bare && list[0].is_main && list[0].head.is_empty());
        assert!(list[1].locked);
        assert!(parse_list(b"").unwrap().is_empty());
    }

    #[test]
    fn rejects_orphan_attribute() {
        assert!(matches!(parse_list(b"HEAD abcd\0\0"), Err(Error::Parse(_))));
    }
}
