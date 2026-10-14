//! Ports: what the application layer needs from the outside world, as plain synchronous
//! traits used through `&dyn` or `Arc<dyn>`. Adapter crates implement them; every call may block
//! on disk or process I/O, so callers run them off the UI thread.

use std::path::{Path, PathBuf};

use crate::changes::{DiffOptions, FileDiff, GitDirs, Status};
use crate::{
    Project, ProjectId, Session, SessionId, SessionStatus, Workspace, WorkspaceId, WorkspaceStatus,
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
}
