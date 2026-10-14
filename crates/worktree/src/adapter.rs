//! [`GitCli`]: the git CLI behind `hangar_core`'s [`VersionControl`] port.

use std::path::{Path, PathBuf};

use hangar_core::ports::{
    ChangeCounts, ListedWorktree, NewWorktree, PortError, PortResult, VersionControl,
};

use crate::{ChangeKind, CreateOptions, Git};

/// Runs `git` for every call; stateless.
#[derive(Clone, Copy, Debug, Default)]
pub struct GitCli;

impl VersionControl for GitCli {
    fn repo_root(&self, path: &Path) -> PortResult<PathBuf> {
        crate::repo_root(path).map_err(PortError::new)
    }

    fn default_branch(&self, repo: &Path) -> PortResult<Option<String>> {
        crate::default_branch(repo).map_err(PortError::new)
    }

    fn is_valid_branch_name(&self, repo: &Path, name: &str) -> PortResult<bool> {
        crate::is_valid_branch_name(repo, name).map_err(PortError::new)
    }

    fn branch_exists(&self, repo: &Path, name: &str) -> PortResult<bool> {
        crate::branch_exists(repo, name).map_err(PortError::new)
    }

    fn add_worktree(&self, repo: &Path, worktree: &NewWorktree) -> PortResult<PathBuf> {
        let options = CreateOptions {
            branch: worktree.branch.clone(),
            base: Some(worktree.base.clone()),
            path: worktree.path.clone(),
            reuse_existing_branch: false,
        };
        crate::create(repo, &options)
            .map(|created| created.path)
            .map_err(PortError::new)
    }

    fn worktrees(&self, repo: &Path) -> PortResult<Vec<ListedWorktree>> {
        let listed = crate::list(repo).map_err(PortError::new)?;
        Ok(listed
            .into_iter()
            .map(|wt| ListedWorktree {
                path: wt.path,
                is_main: wt.is_main,
                bare: wt.bare,
            })
            .collect())
    }

    fn remove_worktree(&self, repo: &Path, path: &Path, force: bool) -> PortResult<()> {
        crate::remove(repo, path, force).map_err(PortError::new)
    }

    fn prune_worktrees(&self, repo: &Path) -> PortResult<()> {
        crate::prune(repo).map_err(PortError::new)
    }

    fn delete_branch(&self, repo: &Path, branch: &str, force: bool) -> PortResult<()> {
        crate::delete_branch(repo, branch, force).map_err(PortError::new)
    }

    fn change_counts(&self, worktree: &Path) -> PortResult<ChangeCounts> {
        let status = crate::status(worktree).map_err(PortError::new)?;
        Ok(ChangeCounts {
            uncommitted: status
                .changes
                .iter()
                .filter(|c| c.kind != ChangeKind::Untracked)
                .count(),
            untracked: status.untracked,
        })
    }

    fn commits_ahead(&self, worktree: &Path, base: &str) -> PortResult<u32> {
        let out = Git::new(worktree)
            .run(["rev-list", "--count", &format!("{base}..HEAD")])
            .map_err(PortError::new)?;
        out.trim()
            .parse()
            .map_err(|_| PortError::new(format!("unexpected `git rev-list --count` output: {out}")))
    }
}
