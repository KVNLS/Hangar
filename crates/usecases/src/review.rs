//! Reviewing a workspace: what the toolbar shows, opening a pull request (publishing the branch
//! first when needed) and merging into the base.

use std::path::PathBuf;

use hangar_core::Workspace;
use hangar_core::ports::{CodeHost, ReviewOps};
use hangar_core::review::{CreatePr, GhStatus, MergeOutcome, MergeStrategy, PrInfo, ReviewError};

/// What the review panel acts on: the workspace and its project's repository.
#[derive(Clone, Debug, PartialEq)]
pub struct ReviewTarget {
    pub workspace: Workspace,
    pub repo: PathBuf,
}

impl ReviewTarget {
    /// Local branch to merge into and to open pull requests against.
    pub fn base_branch(&self) -> String {
        local_base(&self.workspace.base)
    }
}

/// `origin/main` → `main`. Only the `origin/` prefix is stripped: other remotes are rare for a
/// workspace base and `feature/x` must stay as is.
pub fn local_base(base: &str) -> String {
    base.strip_prefix("origin/").unwrap_or(base).to_owned()
}

/// `gh` is installed and signed in.
pub fn can_open_prs(gh: &GhStatus) -> bool {
    matches!(
        gh,
        GhStatus::Installed {
            authenticated: true,
            ..
        }
    )
}

/// What the toolbar needs beyond the live status.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReviewInfo {
    pub gh: GhStatus,
    pub pr: Option<PrInfo>,
    /// Why the PR lookup failed (network, auth); the toolbar still works.
    pub pr_error: Option<String>,
    /// Subject of the branch's newest commit since its base.
    pub last_subject: Option<String>,
    /// The repository has at least one remote (push and pull requests need one).
    pub remote: bool,
    /// `false` once `gh` said none of the remotes is on GitHub (pull requests cannot open).
    pub github: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PrRequest {
    pub title: String,
    pub body: String,
    pub draft: bool,
}

#[derive(Clone, Copy)]
pub struct Reviews<'a> {
    git: &'a dyn ReviewOps,
    host: &'a dyn CodeHost,
}

impl<'a> Reviews<'a> {
    pub fn new(git: &'a dyn ReviewOps, host: &'a dyn CodeHost) -> Self {
        Self { git, host }
    }

    /// Never fails: what cannot be read is logged and left out.
    pub fn load_info(&self, target: &ReviewTarget) -> ReviewInfo {
        let ws = &target.workspace;
        let last_subject = self
            .git
            .last_subject(&ws.worktree_path, &ws.base)
            .ok()
            .flatten();
        let remote = self.git.has_remote(&ws.worktree_path).unwrap_or(true);
        let gh = self.host.status().unwrap_or_else(|err| {
            tracing::warn!(%err, "gh auth status failed");
            GhStatus::Missing
        });
        let mut github = true;
        let (pr, pr_error) = if remote && can_open_prs(&gh) {
            match self.host.pr_for_branch(&ws.worktree_path, &ws.branch) {
                Ok(pr) => (pr, None),
                Err(ReviewError::NotGitHub) => {
                    github = false;
                    (None, None)
                }
                Err(err) => {
                    tracing::warn!(%err, branch = %ws.branch, "gh pr view failed");
                    (None, Some(err.to_string()))
                }
            }
        } else {
            (None, None)
        };
        ReviewInfo {
            gh,
            pr,
            pr_error,
            last_subject,
            remote,
            github,
        }
    }

    /// Pushes the branch if the remote lacks commits, then opens the PR. Returns its URL.
    pub fn create_pr(
        &self,
        target: &ReviewTarget,
        request: &PrRequest,
    ) -> Result<String, ReviewError> {
        let ws = &target.workspace;
        if request.title.trim().is_empty() {
            return Err(ReviewError::EmptyPrTitle);
        }
        // Nothing is pushed for a pull request that cannot be opened: `gh` must be signed in
        // and see a GitHub remote.
        match self.host.status()? {
            GhStatus::Missing => return Err(ReviewError::GhMissing),
            gh if !can_open_prs(&gh) => return Err(ReviewError::GhSignedOut),
            _ => {}
        }
        self.host.pr_for_branch(&ws.worktree_path, &ws.branch)?;
        let upstream = self.git.upstream(&ws.worktree_path)?;
        if upstream.as_ref().is_none_or(|u| u.ahead > 0 || u.gone) {
            self.git
                .push(&ws.worktree_path)
                .map_err(|err| ReviewError::PushBeforePr(Box::new(err)))?;
        }
        self.host.create_pr(
            &ws.worktree_path,
            &CreatePr {
                base: target.base_branch(),
                head: ws.branch.clone(),
                title: request.title.trim().to_owned(),
                body: request.body.clone(),
                draft: request.draft,
            },
        )
    }
}

/// Merges the workspace branch into its local base branch. Returns the new commit on the base.
pub fn merge(
    git: &dyn ReviewOps,
    target: &ReviewTarget,
    strategy: MergeStrategy,
    message: &str,
) -> Result<MergeOutcome, ReviewError> {
    git.merge_into_base(
        &target.repo,
        &target.workspace.branch,
        &target.base_branch(),
        strategy,
        message,
    )
}

#[cfg(test)]
mod tests;
