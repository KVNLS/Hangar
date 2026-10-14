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
        let (pr, pr_error) = if remote && can_open_prs(&gh) {
            match self.host.pr_for_branch(&ws.worktree_path, &ws.branch) {
                Ok(pr) => (pr, None),
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
mod tests {
    use std::path::Path;
    use std::sync::Mutex;

    use hangar_core::review::{PullOutcome, PushOutcome, Upstream};
    use hangar_core::{Timestamp, WorkspaceId, WorkspaceStatus};

    use super::*;

    /// Records the git and gh calls; `upstream` is what the branch tracks.
    #[derive(Default)]
    struct Fake {
        upstream: Option<Upstream>,
        gh: Option<GhStatus>,
        calls: Mutex<Vec<String>>,
    }

    impl Fake {
        fn record(&self, call: impl Into<String>) {
            self.calls.lock().unwrap().push(call.into());
        }
        fn calls(&self) -> Vec<String> {
            self.calls.lock().unwrap().clone()
        }
    }

    impl ReviewOps for Fake {
        fn stage(&self, _: &Path, _: &[PathBuf]) -> Result<(), ReviewError> {
            unimplemented!()
        }
        fn unstage(&self, _: &Path, _: &[PathBuf]) -> Result<(), ReviewError> {
            unimplemented!()
        }
        fn stage_all(&self, _: &Path) -> Result<(), ReviewError> {
            unimplemented!()
        }
        fn commit(&self, _: &Path, _: &str) -> Result<String, ReviewError> {
            unimplemented!()
        }
        fn upstream(&self, _: &Path) -> Result<Option<Upstream>, ReviewError> {
            Ok(self.upstream.clone())
        }
        fn push(&self, _: &Path) -> Result<PushOutcome, ReviewError> {
            self.record("push");
            Ok(PushOutcome {
                remote: "origin".into(),
                branch: "hangar/x".into(),
                set_upstream: true,
                up_to_date: false,
            })
        }
        fn pull_rebase(&self, _: &Path) -> Result<PullOutcome, ReviewError> {
            unimplemented!()
        }
        fn merge_into_base(
            &self,
            _: &Path,
            branch: &str,
            base: &str,
            _: MergeStrategy,
            _: &str,
        ) -> Result<MergeOutcome, ReviewError> {
            self.record(format!("merge {branch} into {base}"));
            Ok(MergeOutcome {
                worktree: "/repo".into(),
                head: "abc".into(),
            })
        }
        fn last_subject(&self, _: &Path, _: &str) -> Result<Option<String>, ReviewError> {
            Ok(Some("Fix login".into()))
        }
        fn has_remote(&self, _: &Path) -> Result<bool, ReviewError> {
            Ok(true)
        }
    }

    impl CodeHost for Fake {
        fn status(&self) -> Result<GhStatus, ReviewError> {
            self.gh.clone().ok_or(ReviewError::GhMissing)
        }
        fn pr_for_branch(&self, _: &Path, branch: &str) -> Result<Option<PrInfo>, ReviewError> {
            self.record(format!("pr view {branch}"));
            Ok(None)
        }
        fn create_pr(&self, _: &Path, request: &CreatePr) -> Result<String, ReviewError> {
            self.record(format!(
                "pr create {} -> {}: {}",
                request.head, request.base, request.title
            ));
            Ok("https://example.invalid/pr/1".into())
        }
    }

    fn target() -> ReviewTarget {
        ReviewTarget {
            workspace: Workspace {
                id: WorkspaceId::new(),
                project_id: hangar_core::ProjectId::new(),
                name: "x".into(),
                slug: "x".into(),
                branch: "hangar/x".into(),
                base: "origin/main".into(),
                worktree_path: "/wt/x".into(),
                status: WorkspaceStatus::Active,
                created_at: Timestamp::now(),
            },
            repo: "/repo".into(),
        }
    }

    fn request(title: &str) -> PrRequest {
        PrRequest {
            title: title.into(),
            body: String::new(),
            draft: false,
        }
    }

    fn upstream(ahead: u32) -> Upstream {
        Upstream {
            name: "origin/hangar/x".into(),
            remote: "origin".into(),
            remote_ref: "refs/heads/hangar/x".into(),
            ahead,
            behind: 0,
            gone: false,
        }
    }

    #[test]
    fn a_pull_request_publishes_the_branch_first_when_needed() {
        let fake = Fake::default();
        let url = Reviews::new(&fake, &fake)
            .create_pr(&target(), &request(" Fix login "))
            .unwrap();
        assert_eq!(url, "https://example.invalid/pr/1");
        assert_eq!(
            fake.calls(),
            ["push", "pr create hangar/x -> main: Fix login"]
        );

        let published = Fake {
            upstream: Some(upstream(0)),
            ..Fake::default()
        };
        Reviews::new(&published, &published)
            .create_pr(&target(), &request("t"))
            .unwrap();
        assert_eq!(published.calls(), ["pr create hangar/x -> main: t"]);
    }

    #[test]
    fn an_empty_title_is_refused_before_anything_runs() {
        let fake = Fake {
            upstream: Some(upstream(2)),
            ..Fake::default()
        };
        let err = Reviews::new(&fake, &fake)
            .create_pr(&target(), &request("  "))
            .unwrap_err();
        assert!(matches!(err, ReviewError::EmptyPrTitle));
        assert!(fake.calls().is_empty());
    }

    #[test]
    fn info_skips_the_pr_lookup_without_a_signed_in_gh() {
        let missing = Fake::default();
        let info = Reviews::new(&missing, &missing).load_info(&target());
        assert_eq!(info.gh, GhStatus::Missing);
        assert_eq!(info.last_subject.as_deref(), Some("Fix login"));
        assert!(missing.calls().is_empty());

        let signed_in = Fake {
            gh: Some(GhStatus::Installed {
                authenticated: true,
                account: None,
            }),
            ..Fake::default()
        };
        Reviews::new(&signed_in, &signed_in).load_info(&target());
        assert_eq!(signed_in.calls(), ["pr view hangar/x"]);
    }

    #[test]
    fn merges_into_the_local_base() {
        let fake = Fake::default();
        merge(&fake, &target(), MergeStrategy::NoFf, "m").unwrap();
        assert_eq!(fake.calls(), ["merge hangar/x into main"]);
    }
}
