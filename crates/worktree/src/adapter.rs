//! The git and `gh` CLIs behind `hangar_core`'s ports: [`GitCli`] for [`VersionControl`],
//! [`Changes`] and [`ReviewOps`], [`Gh`] for [`CodeHost`].

use std::path::{Path, PathBuf};

use hangar_core::changes::{DiffOptions, FileDiff, GitDirs, Status};
use hangar_core::ports::{
    ChangeCounts, Changes, CodeHost, ListedWorktree, NewWorktree, PortError, PortResult, ReviewOps,
    VersionControl,
};
use hangar_core::review::{
    CreatePr, GhStatus, MergeOutcome, MergeStrategy, PrInfo, PullOutcome, PushOutcome, ReviewError,
    Upstream,
};

use crate::{ChangeKind, CommitOptions, CreateOptions, Error, Gh, Git};

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

    fn local_branches(&self, repo: &Path) -> PortResult<Vec<String>> {
        crate::local_branches(repo).map_err(PortError::new)
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

impl Changes for GitCli {
    fn status(&self, worktree: &Path, paths: &[PathBuf]) -> PortResult<Status> {
        crate::status_paths(worktree, paths).map_err(PortError::new)
    }

    fn merge_base(&self, worktree: &Path, rev: &str) -> PortResult<String> {
        crate::merge_base(worktree, rev).map_err(PortError::new)
    }

    fn diff(
        &self,
        worktree: &Path,
        rev: &str,
        paths: &[PathBuf],
        options: &DiffOptions,
    ) -> PortResult<Vec<FileDiff>> {
        crate::diff_paths(worktree, rev, paths, options).map_err(PortError::new)
    }

    fn git_dirs(&self, worktree: &Path) -> PortResult<GitDirs> {
        crate::git_dirs(worktree).map_err(PortError::new)
    }

    fn ignored(&self, worktree: &Path, paths: &[PathBuf]) -> PortResult<Vec<PathBuf>> {
        crate::check_ignored(worktree, paths).map_err(PortError::new)
    }

    fn list_files(&self, worktree: &Path) -> PortResult<Vec<PathBuf>> {
        crate::list_files(worktree).map_err(PortError::new)
    }
}

/// Keeps the refusals the review screen explains; everything else stays opaque.
impl From<Error> for ReviewError {
    fn from(err: Error) -> Self {
        match err {
            Error::NothingToCommit => Self::NothingToCommit,
            Error::EmptyMessage => Self::EmptyMessage,
            Error::HookFailed { stderr } => Self::HookFailed { stderr },
            Error::DetachedHead => Self::DetachedHead,
            Error::BranchMissing(branch) => Self::BranchMissing(branch),
            Error::NoRemote => Self::NoRemote,
            Error::PushRejected {
                remote,
                branch,
                reason,
                non_fast_forward,
            } => Self::PushRejected {
                remote,
                branch,
                reason,
                non_fast_forward,
            },
            Error::NoUpstream(branch) => Self::NoUpstream(branch),
            Error::RebaseConflicts { onto, paths } => Self::RebaseConflicts { onto, paths },
            Error::BaseNotReady { reason } => Self::BaseNotReady { reason },
            Error::MergeConflicts {
                branch,
                base,
                paths,
            } => Self::MergeConflicts {
                branch,
                base,
                paths,
            },
            Error::AlreadyMerged { branch, base } => Self::AlreadyMerged { branch, base },
            Error::GhMissing => Self::GhMissing,
            other => Self::Other(PortError::new(other)),
        }
    }
}

impl ReviewOps for GitCli {
    fn stage(&self, worktree: &Path, paths: &[PathBuf]) -> Result<(), ReviewError> {
        Ok(crate::stage(worktree, paths)?)
    }

    fn unstage(&self, worktree: &Path, paths: &[PathBuf]) -> Result<(), ReviewError> {
        Ok(crate::unstage(worktree, paths)?)
    }

    fn stage_all(&self, worktree: &Path) -> Result<(), ReviewError> {
        Ok(crate::stage_all(worktree)?)
    }

    fn commit(&self, worktree: &Path, message: &str) -> Result<String, ReviewError> {
        Ok(crate::commit(worktree, message, CommitOptions::default())?)
    }

    fn upstream(&self, worktree: &Path) -> Result<Option<Upstream>, ReviewError> {
        Ok(crate::upstream(worktree)?)
    }

    fn push(&self, worktree: &Path) -> Result<PushOutcome, ReviewError> {
        Ok(crate::push(worktree, None)?)
    }

    fn pull_rebase(&self, worktree: &Path) -> Result<PullOutcome, ReviewError> {
        Ok(crate::pull_rebase(worktree)?)
    }

    fn merge_into_base(
        &self,
        repo: &Path,
        branch: &str,
        base: &str,
        strategy: MergeStrategy,
        message: &str,
    ) -> Result<MergeOutcome, ReviewError> {
        Ok(crate::merge_into_base(
            repo, branch, base, strategy, message,
        )?)
    }

    fn last_subject(&self, worktree: &Path, base: &str) -> Result<Option<String>, ReviewError> {
        let out = Git::new(worktree).run(["log", "-1", "--format=%s", &format!("{base}..HEAD")])?;
        Ok(Some(out.trim().to_owned()).filter(|s| !s.is_empty()))
    }

    fn has_remote(&self, worktree: &Path) -> Result<bool, ReviewError> {
        Ok(!Git::new(worktree).run(["remote"])?.trim().is_empty())
    }
}

impl CodeHost for Gh {
    fn status(&self) -> Result<GhStatus, ReviewError> {
        Ok(Gh::status(self)?)
    }

    fn pr_for_branch(&self, worktree: &Path, branch: &str) -> Result<Option<PrInfo>, ReviewError> {
        Ok(Gh::pr_for_branch(self, worktree, branch)?)
    }

    fn create_pr(&self, worktree: &Path, request: &CreatePr) -> Result<String, ReviewError> {
        Ok(Gh::create_pr(self, worktree, request)?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn review_refusals_keep_their_shape_and_wording() {
        let git = Error::PushRejected {
            remote: "origin".into(),
            branch: "b".into(),
            reason: "fetch first".into(),
            non_fast_forward: true,
        };
        let text = git.to_string();
        let review = ReviewError::from(git);
        assert!(matches!(
            review,
            ReviewError::PushRejected {
                non_fast_forward: true,
                ..
            }
        ));
        assert_eq!(review.to_string(), text);

        let other = ReviewError::from(Error::Parse("x".into()));
        assert!(matches!(other, ReviewError::Other(_)));
        assert_eq!(other.to_string(), "unexpected git output: x");
    }
}
