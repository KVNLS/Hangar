//! Review actions behind the changes panel: stage, commit, push, pull request and merge.
//! Text helpers are pure; the `run_*` functions block on git/gh and must run on the
//! background executor.

use std::path::{Path, PathBuf};

use anyhow::Context as _;
use hangar_core::Workspace;
use hangar_worktree::{
    ChangedPath, CommitOptions, CreatePr, Error as GitError, Gh, GhStatus, MergeStrategy, PrInfo,
    Status,
};

/// What the panel acts on: the workspace and its project's repository.
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

/// Staging state of one listed file, from the worktree status.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StageState {
    /// Committed on the branch only: nothing to stage.
    Committed,
    Unstaged,
    Partial,
    Staged,
}

impl StageState {
    pub fn of(change: Option<&ChangedPath>) -> Self {
        match change {
            None => Self::Committed,
            Some(c) if c.staged && c.unstaged => Self::Partial,
            Some(c) if c.staged => Self::Staged,
            Some(_) => Self::Unstaged,
        }
    }

    /// The checkbox shows checked only when everything in the file is staged.
    pub fn checked(self) -> bool {
        self == Self::Staged
    }
}

/// Paths a stage/unstage click touches: the file and, for a rename, its source.
pub fn stage_paths(change: &ChangedPath) -> Vec<PathBuf> {
    let mut paths = vec![change.path.clone()];
    paths.extend(change.orig_path.clone());
    paths
}

/// Why Commit is disabled, if it is.
pub fn commit_blocker(message: &str, staged: usize) -> Option<&'static str> {
    if staged == 0 {
        Some("Stage files to commit")
    } else if message.trim().is_empty() {
        Some("Write a commit message")
    } else {
        None
    }
}

/// The branch's upstream when it is the same-named remote branch (where Push goes). A branch
/// tracking its base (`origin/main`) counts as unpublished.
pub fn own_upstream(status: &Status) -> Option<&str> {
    let branch = status.branch.as_deref()?;
    status
        .upstream
        .as_deref()
        .filter(|up| up.ends_with(&format!("/{branch}")))
}

/// Push button label: `Publish` before the first push, then `Push ↑2 ↓1`.
pub fn push_label(status: &Status) -> String {
    if own_upstream(status).is_none() {
        return "Publish".to_owned();
    }
    let mut label = "Push".to_owned();
    if status.ahead > 0 {
        label.push_str(&format!(" ↑{}", status.ahead));
    }
    if status.behind > 0 {
        label.push_str(&format!(" ↓{}", status.behind));
    }
    label
}

pub fn push_tooltip(status: &Status) -> String {
    match own_upstream(status) {
        None => "Push the branch and set its upstream".to_owned(),
        Some(up) if status.ahead == 0 && status.behind == 0 => format!("Up to date with {up}"),
        Some(up) => format!(
            "{} commit(s) to push, {} to pull ({up})",
            status.ahead, status.behind
        ),
    }
}

/// Why pull requests are unavailable, if they are.
pub fn gh_blocker(gh: &GhStatus) -> Option<String> {
    match gh {
        GhStatus::Missing => Some("Install the GitHub CLI (gh) to create pull requests".into()),
        GhStatus::Installed {
            authenticated: false,
            ..
        } => Some("Run `gh auth login` to create pull requests".into()),
        GhStatus::Installed { .. } => None,
    }
}

/// PR title: the branch's last commit subject, else the workspace name.
pub fn default_pr_title(last_subject: Option<&str>, workspace_name: &str) -> String {
    last_subject
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or(workspace_name)
        .to_owned()
}

pub fn default_merge_message(
    strategy: MergeStrategy,
    branch: &str,
    base: &str,
    title: &str,
) -> String {
    match strategy {
        MergeStrategy::NoFf => format!("Merge branch '{branch}' into {base}"),
        MergeStrategy::Squash => title.to_owned(),
    }
}

/// `#12 · open` for the PR chip.
pub fn pr_label(pr: &PrInfo) -> String {
    format!("#{} · {}", pr.number, pr.state.to_lowercase())
}

/// Errors the user can act on, worded for the UI; everything else is shown in full.
pub fn describe(err: &anyhow::Error) -> String {
    match err.downcast_ref::<GitError>() {
        Some(GitError::MergeConflicts { paths, base, .. }) => {
            let shown: Vec<String> = paths
                .iter()
                .take(8)
                .map(|p| p.display().to_string())
                .collect();
            let more = paths.len().saturating_sub(shown.len());
            let mut text = format!("Merging would conflict with {base} in {}", shown.join(", "));
            if more > 0 {
                text.push_str(&format!(" and {more} more"));
            }
            text.push_str(". Rebase or merge the base into the workspace first.");
            text
        }
        Some(err) => err.to_string(),
        None => format!("{err:#}"),
    }
}

/// Merge refusals shown inside the dialog rather than as a notification.
pub fn is_merge_refusal(err: &anyhow::Error) -> bool {
    matches!(
        err.downcast_ref::<GitError>(),
        Some(
            GitError::BaseNotReady { .. }
                | GitError::MergeConflicts { .. }
                | GitError::AlreadyMerged { .. }
                | GitError::EmptyMessage
        )
    )
}

/// What the toolbar needs beyond the live status.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReviewInfo {
    pub gh: GhStatus,
    pub pr: Option<PrInfo>,
    /// Why the PR lookup failed (network, auth); the toolbar still works.
    pub pr_error: Option<String>,
    pub last_subject: Option<String>,
    /// The repository has at least one remote (push and pull requests need one).
    pub remote: bool,
}

/// Why push and pull requests are unavailable for lack of a remote.
pub const NO_REMOTE: &str = "Add a Git remote to publish this branch and open pull requests";

pub fn load_info(gh: &Gh, target: &ReviewTarget) -> ReviewInfo {
    let ws = &target.workspace;
    let last_subject = hangar_worktree::Git::new(&ws.worktree_path)
        .run(["log", "-1", "--format=%s", &format!("{}..HEAD", ws.base)])
        .ok()
        .map(|s| s.trim().to_owned())
        .filter(|s| !s.is_empty());
    let remote = hangar_worktree::Git::new(&ws.worktree_path)
        .run(["remote"])
        .map(|out| !out.trim().is_empty())
        .unwrap_or(true);
    let gh_status = gh.status().unwrap_or_else(|err| {
        tracing::warn!(%err, "gh auth status failed");
        GhStatus::Missing
    });
    let (pr, pr_error) = if remote && gh_blocker(&gh_status).is_none() {
        match gh.pr_for_branch(&ws.worktree_path, &ws.branch) {
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
        gh: gh_status,
        pr,
        pr_error,
        last_subject,
        remote,
    }
}

pub fn run_stage(worktree: &Path, paths: &[PathBuf], stage: bool) -> anyhow::Result<()> {
    if stage {
        hangar_worktree::stage(worktree, paths).context("failed to stage")
    } else {
        hangar_worktree::unstage(worktree, paths).context("failed to unstage")
    }
}

pub fn run_stage_all(worktree: &Path) -> anyhow::Result<()> {
    hangar_worktree::stage_all(worktree).context("failed to stage all changes")
}

/// Returns the new HEAD.
pub fn run_commit(worktree: &Path, message: &str) -> anyhow::Result<String> {
    Ok(hangar_worktree::commit(
        worktree,
        message,
        CommitOptions::default(),
    )?)
}

pub fn run_push(worktree: &Path) -> anyhow::Result<hangar_worktree::PushOutcome> {
    Ok(hangar_worktree::push(worktree, None)?)
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PrRequest {
    pub title: String,
    pub body: String,
    pub draft: bool,
}

/// Pushes the branch if the remote lacks commits, then opens the PR. Returns its URL.
pub fn run_create_pr(
    gh: &Gh,
    target: &ReviewTarget,
    request: &PrRequest,
) -> anyhow::Result<String> {
    let ws = &target.workspace;
    if request.title.trim().is_empty() {
        anyhow::bail!("the pull request title is empty");
    }
    let upstream = hangar_worktree::upstream(&ws.worktree_path)?;
    if upstream.as_ref().is_none_or(|u| u.ahead > 0 || u.gone) {
        hangar_worktree::push(&ws.worktree_path, None)
            .context("failed to push before opening the pull request")?;
    }
    let url = gh.create_pr(
        &ws.worktree_path,
        &CreatePr {
            base: target.base_branch(),
            head: ws.branch.clone(),
            title: request.title.trim().to_owned(),
            body: request.body.clone(),
            draft: request.draft,
        },
    )?;
    Ok(url)
}

/// Returns the new commit on the base branch.
pub fn run_merge(
    target: &ReviewTarget,
    strategy: MergeStrategy,
    message: &str,
) -> anyhow::Result<hangar_worktree::MergeOutcome> {
    Ok(hangar_worktree::merge_into_base(
        &target.repo,
        &target.workspace.branch,
        &target.base_branch(),
        strategy,
        message,
    )?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use hangar_worktree::ChangeKind;

    fn change(staged: bool, unstaged: bool) -> ChangedPath {
        ChangedPath {
            path: "new.rs".into(),
            orig_path: Some("old.rs".into()),
            kind: ChangeKind::Renamed,
            staged,
            unstaged,
        }
    }

    #[test]
    fn stage_states() {
        assert_eq!(StageState::of(None), StageState::Committed);
        assert_eq!(
            StageState::of(Some(&change(true, false))),
            StageState::Staged
        );
        assert_eq!(
            StageState::of(Some(&change(true, true))),
            StageState::Partial
        );
        assert_eq!(
            StageState::of(Some(&change(false, true))),
            StageState::Unstaged
        );
        assert!(StageState::Staged.checked() && !StageState::Partial.checked());
        assert_eq!(
            stage_paths(&change(true, false)),
            [PathBuf::from("new.rs"), PathBuf::from("old.rs")]
        );
    }

    #[test]
    fn commit_and_push_text() {
        assert_eq!(commit_blocker("msg", 0), Some("Stage files to commit"));
        assert_eq!(commit_blocker(" \n", 2), Some("Write a commit message"));
        assert_eq!(commit_blocker("msg", 1), None);

        let mut status = Status::default();
        assert_eq!(push_label(&status), "Publish");
        status.branch = Some("hangar/x".into());
        status.upstream = Some("origin/main".into());
        status.ahead = 3;
        assert_eq!(push_label(&status), "Publish");
        status.ahead = 0;
        status.upstream = Some("origin/hangar/x".into());
        assert_eq!(push_label(&status), "Push");
        assert!(push_tooltip(&status).contains("Up to date"));
        status.ahead = 2;
        status.behind = 1;
        assert_eq!(push_label(&status), "Push ↑2 ↓1");
        assert!(push_tooltip(&status).starts_with("2 commit(s) to push, 1"));
    }

    #[test]
    fn pr_and_merge_text() {
        assert_eq!(local_base("origin/main"), "main");
        assert_eq!(local_base("main"), "main");
        assert_eq!(local_base("feature/origin/x"), "feature/origin/x");
        assert_eq!(default_pr_title(Some(" Fix login "), "ws"), "Fix login");
        assert_eq!(default_pr_title(Some(""), "ws"), "ws");
        assert_eq!(default_pr_title(None, "ws"), "ws");
        assert_eq!(
            default_merge_message(MergeStrategy::NoFf, "hangar/x", "main", "T"),
            "Merge branch 'hangar/x' into main"
        );
        assert_eq!(
            default_merge_message(MergeStrategy::Squash, "hangar/x", "main", "T"),
            "T"
        );
        assert!(gh_blocker(&GhStatus::Missing).is_some());
        assert!(
            gh_blocker(&GhStatus::Installed {
                authenticated: false,
                account: None
            })
            .unwrap()
            .contains("gh auth login")
        );
        assert!(
            gh_blocker(&GhStatus::Installed {
                authenticated: true,
                account: None
            })
            .is_none()
        );
        let pr = PrInfo {
            number: 3,
            url: "u".into(),
            state: "OPEN".into(),
            title: "t".into(),
        };
        assert_eq!(pr_label(&pr), "#3 · open");
    }

    #[test]
    fn error_wording() {
        let conflicts = anyhow::Error::new(GitError::MergeConflicts {
            branch: "hangar/x".into(),
            base: "main".into(),
            paths: (0..10).map(|i| PathBuf::from(format!("f{i}"))).collect(),
        });
        let text = describe(&conflicts);
        assert!(
            text.contains("f0, f1") && text.contains("and 2 more"),
            "{text}"
        );
        assert!(is_merge_refusal(&conflicts));
        let ready = anyhow::Error::new(GitError::BaseNotReady {
            reason: "dirty".into(),
        });
        assert!(is_merge_refusal(&ready));
        assert!(describe(&ready).contains("dirty"));
        assert!(!is_merge_refusal(&anyhow::anyhow!("other")));
    }
}
