//! Review actions behind the changes panel: stage, commit, push, pull request and merge.
//! Text helpers are pure; the `run_*` functions block on git/gh and must run on the
//! background executor.

use std::path::{Path, PathBuf};

use anyhow::Context as _;
use hangar_core::AgentKind;
use hangar_core::changes::{ChangedPath, Status};
use hangar_core::ports::CodeHost;
use hangar_core::review::{
    GhStatus, MergeOutcome, MergeStrategy, PrInfo, PushOutcome, ReviewError,
};
use hangar_usecases::review::Reviews;
pub use hangar_usecases::review::{PrRequest, ReviewInfo, ReviewTarget, local_base};

use crate::adapters;

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

/// What the diff is compared against: the base branch's name, else the merge-base commit.
pub fn base_label(branch: Option<&str>, merge_base: &str) -> String {
    match branch.map(str::trim).filter(|b| !b.is_empty()) {
        Some(branch) => branch.to_owned(),
        None => crate::changes::model::short_sha(merge_base).to_owned(),
    }
}

/// The branch the workspace base names (`origin/main` → `main`); `None` for a commit. Blocks.
pub fn resolve_base_name(worktree: &Path, base: &str) -> Option<String> {
    hangar_worktree::base_branch_name(worktree, base)
        .inspect_err(|err| tracing::warn!(%err, base, "could not resolve the base branch"))
        .ok()
        .flatten()
}

/// Why Commit is disabled, if it is.
pub fn commit_blocker(subject: &str, staged: usize) -> Option<&'static str> {
    if staged == 0 {
        Some("Stage files to commit")
    } else if subject.trim().is_empty() {
        Some("Write a commit subject")
    } else {
        None
    }
}

/// `Co-authored-by: Claude <noreply@anthropic.com>` for an agent preset; `None` for a shell or
/// a custom command (no one to credit).
pub fn coauthor_trailer(agent: &AgentKind) -> Option<String> {
    let (name, email) = match agent {
        AgentKind::Claude => ("Claude", "noreply@anthropic.com"),
        AgentKind::Codex => ("Codex", "noreply@openai.com"),
        AgentKind::OpenCode => ("OpenCode", "noreply@opencode.ai"),
        AgentKind::Gemini => ("Gemini", "noreply@google.com"),
        AgentKind::Shell | AgentKind::Custom(_) => return None,
    };
    Some(format!("Co-authored-by: {name} <{email}>"))
}

/// The commit message: subject, body and trailer separated by blank lines. The trailer is left
/// out when the body already has it.
pub fn commit_message(subject: &str, body: &str, trailer: Option<&str>) -> String {
    let mut message = subject.trim().to_owned();
    let body = body.trim();
    if !body.is_empty() {
        message.push_str("\n\n");
        message.push_str(body);
    }
    if let Some(trailer) = trailer.map(str::trim).filter(|t| !t.is_empty())
        && !body
            .lines()
            .any(|line| line.trim().eq_ignore_ascii_case(trailer))
    {
        message.push_str("\n\n");
        message.push_str(trailer);
    }
    message
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
    match err.downcast_ref::<ReviewError>() {
        Some(ReviewError::MergeConflicts { paths, base, .. }) => {
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
        err.downcast_ref::<ReviewError>(),
        Some(
            ReviewError::BaseNotReady { .. }
                | ReviewError::MergeConflicts { .. }
                | ReviewError::AlreadyMerged { .. }
                | ReviewError::EmptyMessage
        )
    )
}

/// Why push and pull requests are unavailable for lack of a remote.
pub const NO_REMOTE: &str = "Add a Git remote to publish this branch and open pull requests";

pub fn load_info(gh: &dyn CodeHost, target: &ReviewTarget) -> ReviewInfo {
    Reviews::new(adapters::review_ops(), gh).load_info(target)
}

pub fn run_stage(worktree: &Path, paths: &[PathBuf], stage: bool) -> anyhow::Result<()> {
    let git = adapters::review_ops();
    if stage {
        git.stage(worktree, paths).context("failed to stage")
    } else {
        git.unstage(worktree, paths).context("failed to unstage")
    }
}

pub fn run_stage_all(worktree: &Path) -> anyhow::Result<()> {
    adapters::review_ops()
        .stage_all(worktree)
        .context("failed to stage all changes")
}

/// Returns the new HEAD.
pub fn run_commit(worktree: &Path, message: &str) -> anyhow::Result<String> {
    Ok(adapters::review_ops().commit(worktree, message)?)
}

pub fn run_push(worktree: &Path) -> anyhow::Result<PushOutcome> {
    Ok(adapters::review_ops().push(worktree)?)
}

pub fn run_pull_rebase(worktree: &Path) -> anyhow::Result<hangar_core::review::PullOutcome> {
    Ok(adapters::review_ops().pull_rebase(worktree)?)
}

/// Pushes the branch if the remote lacks commits, then opens the PR. Returns its URL.
pub fn run_create_pr(
    gh: &dyn CodeHost,
    target: &ReviewTarget,
    request: &PrRequest,
) -> anyhow::Result<String> {
    Ok(Reviews::new(adapters::review_ops(), gh).create_pr(target, request)?)
}

/// Returns the new commit on the base branch.
pub fn run_merge(
    target: &ReviewTarget,
    strategy: MergeStrategy,
    message: &str,
) -> anyhow::Result<MergeOutcome> {
    Ok(hangar_usecases::review::merge(
        adapters::review_ops(),
        target,
        strategy,
        message,
    )?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::changes::model::short_sha;
    use hangar_core::changes::ChangeKind;

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
    fn base_labels() {
        let sha = "0123456789abcdef0123456789abcdef01234567";
        assert_eq!(base_label(Some("main"), sha), "main");
        assert_eq!(base_label(Some(" "), sha), short_sha(sha));
        assert_eq!(base_label(None, sha), short_sha(sha));
    }

    #[test]
    fn commit_and_push_text() {
        assert_eq!(commit_blocker("msg", 0), Some("Stage files to commit"));
        assert_eq!(commit_blocker(" \n", 2), Some("Write a commit subject"));
        assert_eq!(commit_blocker("msg", 1), None);

        let claude = coauthor_trailer(&AgentKind::Claude);
        assert_eq!(
            claude.as_deref(),
            Some("Co-authored-by: Claude <noreply@anthropic.com>")
        );
        assert!(
            coauthor_trailer(&AgentKind::Codex)
                .unwrap()
                .contains("Codex <")
        );
        assert_eq!(coauthor_trailer(&AgentKind::Shell), None);
        assert_eq!(coauthor_trailer(&AgentKind::Custom("aider".into())), None);
        let trailer = claude.as_deref();
        assert_eq!(commit_message(" Fix it ", "", None), "Fix it");
        assert_eq!(
            commit_message("Fix it", " Why.\n", trailer),
            "Fix it\n\nWhy.\n\nCo-authored-by: Claude <noreply@anthropic.com>"
        );
        assert_eq!(
            commit_message("Fix it", "", trailer),
            "Fix it\n\nCo-authored-by: Claude <noreply@anthropic.com>"
        );
        let signed = "Why.\n\nco-authored-by: Claude <noreply@anthropic.com>";
        assert_eq!(
            commit_message("Fix it", signed, trailer),
            format!("Fix it\n\n{signed}")
        );

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
        let conflicts = anyhow::Error::new(ReviewError::MergeConflicts {
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
        let ready = anyhow::Error::new(ReviewError::BaseNotReady {
            reason: "dirty".into(),
        });
        assert!(is_merge_refusal(&ready));
        assert!(describe(&ready).contains("dirty"));
        assert!(!is_merge_refusal(&anyhow::anyhow!("other")));
    }
}
