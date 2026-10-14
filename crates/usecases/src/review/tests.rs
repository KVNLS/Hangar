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
            task: None,
            group: None,
            parent: None,
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
