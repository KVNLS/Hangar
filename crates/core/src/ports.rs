//! Ports: what the application layer needs from the outside world, as plain synchronous
//! traits used through `&dyn` or `Arc<dyn>`. Adapter crates implement them; every call may block
//! on disk or process I/O, so callers run them off the UI thread.

use std::path::{Path, PathBuf};

use crate::changes::{Cost, DiffOptions, DiffStat, FileDiff, GitDirs, Status};
use crate::review::{
    CreatePr, GhStatus, MergeOutcome, MergeStrategy, PrInfo, PullOutcome, PushOutcome, ReviewError,
    Upstream,
};
use crate::{
    Project, ProjectId, Session, SessionId, SessionStatus, Timestamp, Workspace, WorkspaceId,
    WorkspaceStatus,
};

/// An adapter's failure, opaque so adapter types (rusqlite, git output) stay out of the domain.
/// Displays exactly as the adapter's own error.
#[derive(Debug, thiserror::Error)]
#[error(transparent)]
pub struct PortError(Box<dyn std::error::Error + Send + Sync>);

impl PortError {
    pub fn new(err: impl Into<Box<dyn std::error::Error + Send + Sync>>) -> Self {
        Self(err.into())
    }
}

pub type PortResult<T> = Result<T, PortError>;

/// Persistence of projects, workspaces and sessions.
pub trait Store: Send + Sync {
    fn insert_project(&self, project: &Project) -> PortResult<()>;
    fn list_projects(&self) -> PortResult<Vec<Project>>;
    fn get_project(&self, id: ProjectId) -> PortResult<Project>;
    /// Also deletes the project's workspaces and sessions.
    fn delete_project(&self, id: ProjectId) -> PortResult<()>;

    fn insert_workspace(&self, workspace: &Workspace) -> PortResult<()>;
    fn list_workspaces(&self, project: ProjectId) -> PortResult<Vec<Workspace>>;
    fn get_workspace(&self, id: WorkspaceId) -> PortResult<Workspace>;
    fn update_workspace_status(&self, id: WorkspaceId, status: WorkspaceStatus) -> PortResult<()>;
    /// Also deletes the workspace's sessions.
    fn delete_workspace(&self, id: WorkspaceId) -> PortResult<()>;

    fn insert_session(&self, session: &Session) -> PortResult<()>;
    fn list_sessions(&self, workspace: WorkspaceId) -> PortResult<Vec<Session>>;
    fn update_session_status(&self, id: SessionId, status: &SessionStatus) -> PortResult<Session>;
    /// Records how a session ended: its status (a terminal one stamps `ended_at` once) and, when
    /// known, the workspace's diff at that moment. Returns the updated session.
    fn finish_session(
        &self,
        id: SessionId,
        status: &SessionStatus,
        changes: Option<DiffStat>,
    ) -> PortResult<Session>;
    /// Sessions of every workspace that ended at or after `since`, the latest ended first.
    fn list_sessions_since(&self, since: Timestamp) -> PortResult<Vec<Session>>;
    /// Records what a session has cost so far.
    fn record_session_cost(&self, id: SessionId, cost: &Cost) -> PortResult<()>;

    /// The JSON stored under `key`; `None` when it was never set.
    fn read_setting(&self, key: &str) -> PortResult<Option<String>>;
    /// Stores `json` under `key`, replacing any previous value.
    fn write_setting(&self, key: &str, json: &str) -> PortResult<()>;
    /// Removing a missing key is not an error.
    fn remove_setting(&self, key: &str) -> PortResult<()>;
}

/// A shared store is a store, so `&Arc<dyn Store>` can be passed where `&dyn Store` is expected.
impl<S: Store + ?Sized> Store for std::sync::Arc<S> {
    fn insert_project(&self, project: &Project) -> PortResult<()> {
        (**self).insert_project(project)
    }
    fn list_projects(&self) -> PortResult<Vec<Project>> {
        (**self).list_projects()
    }
    fn get_project(&self, id: ProjectId) -> PortResult<Project> {
        (**self).get_project(id)
    }
    fn delete_project(&self, id: ProjectId) -> PortResult<()> {
        (**self).delete_project(id)
    }
    fn insert_workspace(&self, workspace: &Workspace) -> PortResult<()> {
        (**self).insert_workspace(workspace)
    }
    fn list_workspaces(&self, project: ProjectId) -> PortResult<Vec<Workspace>> {
        (**self).list_workspaces(project)
    }
    fn get_workspace(&self, id: WorkspaceId) -> PortResult<Workspace> {
        (**self).get_workspace(id)
    }
    fn update_workspace_status(&self, id: WorkspaceId, status: WorkspaceStatus) -> PortResult<()> {
        (**self).update_workspace_status(id, status)
    }
    fn delete_workspace(&self, id: WorkspaceId) -> PortResult<()> {
        (**self).delete_workspace(id)
    }
    fn insert_session(&self, session: &Session) -> PortResult<()> {
        (**self).insert_session(session)
    }
    fn list_sessions(&self, workspace: WorkspaceId) -> PortResult<Vec<Session>> {
        (**self).list_sessions(workspace)
    }
    fn update_session_status(&self, id: SessionId, status: &SessionStatus) -> PortResult<Session> {
        (**self).update_session_status(id, status)
    }
    fn finish_session(
        &self,
        id: SessionId,
        status: &SessionStatus,
        changes: Option<DiffStat>,
    ) -> PortResult<Session> {
        (**self).finish_session(id, status, changes)
    }
    fn list_sessions_since(&self, since: Timestamp) -> PortResult<Vec<Session>> {
        (**self).list_sessions_since(since)
    }
    fn record_session_cost(&self, id: SessionId, cost: &Cost) -> PortResult<()> {
        (**self).record_session_cost(id, cost)
    }
    fn read_setting(&self, key: &str) -> PortResult<Option<String>> {
        (**self).read_setting(key)
    }
    fn write_setting(&self, key: &str, json: &str) -> PortResult<()> {
        (**self).write_setting(key, json)
    }
    fn remove_setting(&self, key: &str) -> PortResult<()> {
        (**self).remove_setting(key)
    }
}

/// A worktree to add on a new branch.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NewWorktree {
    pub branch: String,
    /// Start point of the branch.
    pub base: String,
    pub path: PathBuf,
}

/// A worktree as the repository lists it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ListedWorktree {
    pub path: PathBuf,
    /// The repository's main working tree.
    pub is_main: bool,
    pub bare: bool,
}

/// A local branch and the commit it points at.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BranchTip {
    pub name: String,
    pub short_sha: String,
    /// Committer date of the tip, Unix seconds.
    pub committed_at: i64,
}

/// What removing a worktree would throw away.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ChangeCounts {
    /// Staged, unstaged and conflicted paths.
    pub uncommitted: usize,
    pub untracked: usize,
}

/// The version-control operations the workspace lifecycle needs.
pub trait VersionControl: Send + Sync {
    /// Root of the repository containing `path`.
    fn repo_root(&self, path: &Path) -> PortResult<PathBuf>;
    /// The branch new workspaces start from; `None` when the repository has none yet.
    fn default_branch(&self, repo: &Path) -> PortResult<Option<String>>;
    fn is_valid_branch_name(&self, repo: &Path, name: &str) -> PortResult<bool>;
    fn branch_exists(&self, repo: &Path, name: &str) -> PortResult<bool>;
    /// Local branch names, for picking a workspace's base.
    fn local_branches(&self, repo: &Path) -> PortResult<Vec<String>>;
    /// Local branches with their tip commit, sorted by name.
    fn branch_tips(&self, repo: &Path) -> PortResult<Vec<BranchTip>>;
    /// Creates the branch and its worktree; returns the worktree path as created.
    fn add_worktree(&self, repo: &Path, worktree: &NewWorktree) -> PortResult<PathBuf>;
    fn worktrees(&self, repo: &Path) -> PortResult<Vec<ListedWorktree>>;
    /// `force` discards uncommitted and untracked files.
    fn remove_worktree(&self, repo: &Path, path: &Path, force: bool) -> PortResult<()>;
    fn prune_worktrees(&self, repo: &Path) -> PortResult<()>;
    /// Without `force`, refuses a branch that is not merged.
    fn delete_branch(&self, repo: &Path, branch: &str, force: bool) -> PortResult<()>;
    fn change_counts(&self, worktree: &Path) -> PortResult<ChangeCounts>;
    /// Commits on `HEAD` of `worktree` that are not on `base`.
    fn commits_ahead(&self, worktree: &Path, base: &str) -> PortResult<u32>;
}

/// Reading a worktree's changes: what the file watcher and the live diff need.
pub trait Changes: Send + Sync {
    /// Status limited to `paths` (relative to the worktree, a directory covers everything below
    /// it); empty `paths` means the whole worktree.
    fn status(&self, worktree: &Path, paths: &[PathBuf]) -> PortResult<Status>;
    fn merge_base(&self, worktree: &Path, rev: &str) -> PortResult<String>;
    /// The working tree against the commit `rev`, limited to `paths` as in [`Changes::status`].
    fn diff(
        &self,
        worktree: &Path,
        rev: &str,
        paths: &[PathBuf],
        options: &DiffOptions,
    ) -> PortResult<Vec<FileDiff>>;
    fn git_dirs(&self, worktree: &Path) -> PortResult<GitDirs>;
    /// The subset of `paths` the repository ignores.
    fn ignored(&self, worktree: &Path, paths: &[PathBuf]) -> PortResult<Vec<PathBuf>>;
    /// Tracked and untracked, not ignored, files of the worktree, relative to it.
    fn list_files(&self, worktree: &Path) -> PortResult<Vec<PathBuf>>;
}

/// The git side of reviewing a workspace: stage, commit, publish, merge into the base.
pub trait ReviewOps: Send + Sync {
    fn stage(&self, worktree: &Path, paths: &[PathBuf]) -> Result<(), ReviewError>;
    fn unstage(&self, worktree: &Path, paths: &[PathBuf]) -> Result<(), ReviewError>;
    fn stage_all(&self, worktree: &Path) -> Result<(), ReviewError>;
    /// Commits what is staged; returns the new `HEAD`.
    fn commit(&self, worktree: &Path, message: &str) -> Result<String, ReviewError>;
    /// The same-named remote branch the checked-out branch tracks, if any.
    fn upstream(&self, worktree: &Path) -> Result<Option<Upstream>, ReviewError>;
    /// Pushes the checked-out branch to its same-named remote branch. Never forces.
    fn push(&self, worktree: &Path) -> Result<PushOutcome, ReviewError>;
    /// Rebases local commits onto the fetched upstream; a conflicting rebase is aborted.
    fn pull_rebase(&self, worktree: &Path) -> Result<PullOutcome, ReviewError>;
    /// Merges local `branch` into local `base` where `base` is checked out, or refuses.
    fn merge_into_base(
        &self,
        repo: &Path,
        branch: &str,
        base: &str,
        strategy: MergeStrategy,
        message: &str,
    ) -> Result<MergeOutcome, ReviewError>;
    /// Subject of the newest commit in `base..HEAD`.
    fn last_subject(&self, worktree: &Path, base: &str) -> Result<Option<String>, ReviewError>;
    fn has_remote(&self, worktree: &Path) -> Result<bool, ReviewError>;
}

/// Where pull requests live (GitHub through `gh`).
pub trait CodeHost: Send + Sync {
    fn status(&self) -> Result<GhStatus, ReviewError>;
    fn pr_for_branch(&self, worktree: &Path, branch: &str) -> Result<Option<PrInfo>, ReviewError>;
    /// Opens the pull request; returns its URL.
    fn create_pr(&self, worktree: &Path, request: &CreatePr) -> Result<String, ReviewError>;
}
