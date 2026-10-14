//! Project and workspace lifecycle: register a repository, create a workspace (branch, worktree
//! and row, rolled back together), archive, restore, delete, forget a project.

use std::path::{Path, PathBuf};

use hangar_core::changes::DiffStat;
use hangar_core::ports::{NewWorktree, PortError, Store, VersionControl};
use hangar_core::{
    BRANCH_PREFIX, Project, ProjectId, SessionId, Timestamp, Workspace, WorkspaceGroupId,
    WorkspaceId, WorkspaceStatus, branch_name, slugify, unique_slug, worktree_path,
};

use crate::sessions::stopped;

/// What to create: the name, where its branch starts and how it is named, the task, and the
/// attempts it belongs to. Blank fields mean the defaults.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct NewWorkspace {
    pub name: String,
    /// Branch to start from (default: the project's default base).
    pub base: Option<String>,
    /// The new branch's name (default: `hangar/<slug>`, suffixed until free).
    pub branch: Option<String>,
    pub task: Option<String>,
    pub group: Option<WorkspaceGroupId>,
}

impl NewWorkspace {
    pub fn named(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            ..Self::default()
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum WorkspaceError {
    #[error("{} is not inside a Git repository", .path.display())]
    NotARepository {
        path: PathBuf,
        #[source]
        source: PortError,
    },
    #[error("failed to read branches of {}", .repo.display())]
    Branches {
        repo: PathBuf,
        #[source]
        source: PortError,
    },
    #[error("workspace name is empty")]
    EmptyName,
    #[error("`{0}` is not a valid branch name")]
    InvalidBranch(String),
    #[error("branch `{0}` already exists")]
    BranchExists(String),
    #[error("failed to check existing branches")]
    CheckBranches(#[source] PortError),
    #[error("failed to create worktree {}", .path.display())]
    CreateWorktree {
        path: PathBuf,
        #[source]
        source: PortError,
    },
    #[error("failed to save workspace")]
    SaveWorkspace(#[source] PortError),
    #[error("failed to archive the workspace")]
    Archive(#[source] PortError),
    #[error("failed to restore the workspace")]
    Restore(#[source] PortError),
    #[error("failed to record the stopped session")]
    StopSession(#[source] PortError),
    #[error("failed to read the status of {}", .path.display())]
    Status {
        path: PathBuf,
        #[source]
        source: PortError,
    },
    #[error("failed to list worktrees of {}", .repo.display())]
    ListWorktrees {
        repo: PathBuf,
        #[source]
        source: PortError,
    },
    #[error("failed to remove worktree {}", .path.display())]
    RemoveWorktree {
        path: PathBuf,
        #[source]
        source: PortError,
    },
    #[error("refusing to delete {}: it is the repository's main working tree", .0.display())]
    MainWorktree(PathBuf),
    #[error("refusing to delete {}: it is not a worktree of {}", .path.display(), .repo.display())]
    NotAWorktree { path: PathBuf, repo: PathBuf },
    #[error("the worktree was removed but the workspace could not be deleted")]
    DeleteRow(#[source] PortError),
    #[error("archive or delete its workspaces first: {}", .0.join(", "))]
    ActiveWorkspaces(Vec<String>),
    #[error("failed to remove project")]
    RemoveProject(#[source] PortError),
    #[error(transparent)]
    Port(#[from] PortError),
}

pub type Result<T, E = WorkspaceError> = std::result::Result<T, E>;

/// What deleting a workspace would throw away.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct DeleteSummary {
    /// The worktree directory still exists.
    pub exists: bool,
    /// Staged, unstaged and conflicted paths.
    pub uncommitted: usize,
    pub untracked: usize,
    /// Commits on the workspace branch that are not on its base; `None` if unknown.
    pub ahead: Option<u32>,
}

impl DeleteSummary {
    /// Removing the worktree needs `--force` (and loses work).
    pub fn is_dirty(&self) -> bool {
        self.uncommitted > 0 || self.untracked > 0
    }

    pub fn describe(&self, base: &str) -> Vec<String> {
        if !self.exists {
            return vec!["The worktree directory no longer exists.".to_owned()];
        }
        let mut lines = Vec::new();
        if self.is_dirty() {
            lines.push(format!(
                "{} uncommitted and {} untracked file(s) will be lost.",
                self.uncommitted, self.untracked
            ));
        } else {
            lines.push("No uncommitted or untracked files.".to_owned());
        }
        match self.ahead {
            Some(0) => lines.push(format!("No commits ahead of {base}.")),
            Some(n) => lines.push(format!("{n} commit(s) ahead of {base}.")),
            None => lines.push(format!("Could not count commits ahead of {base}.")),
        }
        lines
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct DeleteOptions {
    /// Discard uncommitted and untracked files. Only set after the user saw them listed.
    pub force: bool,
    /// Also delete the `hangar/<slug>` branch (never forced: unmerged branches are kept).
    pub delete_branch: bool,
}

#[derive(Debug, Default)]
pub struct DeleteOutcome {
    /// The workspace is gone but its branch could not be deleted.
    pub branch_error: Option<String>,
}

/// The lifecycle use cases over a store and a repository.
#[derive(Clone, Copy)]
pub struct Workspaces<'a> {
    store: &'a dyn Store,
    git: &'a dyn VersionControl,
}

impl<'a> Workspaces<'a> {
    pub fn new(store: &'a dyn Store, git: &'a dyn VersionControl) -> Self {
        Self { store, git }
    }

    /// Registers the repository containing `path`, or returns the existing project for it.
    pub fn add_project(&self, path: &Path) -> Result<Project> {
        let root = self
            .git
            .repo_root(path)
            .map_err(|source| WorkspaceError::NotARepository {
                path: path.to_path_buf(),
                source,
            })?;
        if let Some(existing) = self
            .store
            .list_projects()?
            .into_iter()
            .find(|p| p.repo_path == root)
        {
            return Ok(existing);
        }
        let base = self
            .git
            .default_branch(&root)
            .map_err(|source| WorkspaceError::Branches {
                repo: root.clone(),
                source,
            })?
            .unwrap_or_else(|| "HEAD".to_owned());
        let name = root
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| root.display().to_string());
        let project = Project::new(name, root, base);
        self.store.insert_project(&project)?;
        Ok(project)
    }

    /// Creates the branch, the worktree and the workspace row (see [`NewWorkspace`]). The slug
    /// gets a numeric suffix when the name, branch or directory is already taken; a given branch
    /// name is used as is and must not exist yet.
    pub fn create(
        &self,
        project: &Project,
        worktrees_root: &Path,
        spec: &NewWorkspace,
    ) -> Result<Workspace> {
        let (base, branch) = (spec.base.as_deref(), spec.branch.as_deref());
        let task = spec
            .task
            .as_deref()
            .map(str::trim)
            .filter(|t| !t.is_empty());
        let name = spec.name.trim();
        if name.is_empty() {
            return Err(WorkspaceError::EmptyName);
        }
        let repo = &project.repo_path;
        let base = base
            .map(str::trim)
            .filter(|b| !b.is_empty())
            .unwrap_or(&project.default_base)
            .to_owned();
        let chosen = branch.map(str::trim).filter(|b| !b.is_empty());
        if let Some(chosen) = chosen {
            if !self.git.is_valid_branch_name(repo, chosen)? {
                return Err(WorkspaceError::InvalidBranch(chosen.to_owned()));
            }
            if self.git.branch_exists(repo, chosen)? {
                return Err(WorkspaceError::BranchExists(chosen.to_owned()));
            }
        }
        let project_slug = slugify(&project.name);
        let existing: Vec<String> = self
            .store
            .list_workspaces(project.id)?
            .into_iter()
            .map(|ws| ws.slug)
            .collect();
        let mut git_error = None;
        let slug = unique_slug(&slugify(name), |candidate| {
            if existing.iter().any(|s| s == candidate)
                || worktree_path(worktrees_root, &project_slug, candidate).exists()
            {
                return true;
            }
            if chosen.is_some() {
                return false;
            }
            match self.git.branch_exists(repo, &branch_name(candidate)) {
                Ok(taken) => taken,
                Err(err) => {
                    git_error.get_or_insert(err);
                    false
                }
            }
        });
        if let Some(err) = git_error {
            return Err(WorkspaceError::CheckBranches(err));
        }

        let branch = chosen.map_or_else(|| branch_name(&slug), str::to_owned);
        let path = worktree_path(worktrees_root, &project_slug, &slug);
        let new = NewWorktree {
            branch: branch.clone(),
            base: base.clone(),
            path: path.clone(),
        };
        let created =
            self.git
                .add_worktree(repo, &new)
                .map_err(|source| WorkspaceError::CreateWorktree {
                    path: path.clone(),
                    source,
                })?;

        let workspace = Workspace {
            id: WorkspaceId::new(),
            project_id: project.id,
            name: name.to_owned(),
            slug,
            branch,
            base,
            worktree_path: created,
            status: WorkspaceStatus::Active,
            created_at: Timestamp::now(),
            task: task.map(str::to_owned),
            group: spec.group,
        };
        if let Err(err) = self.store.insert_workspace(&workspace) {
            self.rollback(repo, &path, &workspace.branch);
            return Err(WorkspaceError::SaveWorkspace(err));
        }
        Ok(workspace)
    }

    fn rollback(&self, repo: &Path, path: &Path, branch: &str) {
        if let Err(err) = self.git.remove_worktree(repo, path, true) {
            tracing::warn!(%err, "rollback: failed to remove worktree");
        }
        if let Err(err) = self.git.delete_branch(repo, branch, true) {
            tracing::warn!(%err, "rollback: failed to delete branch");
        }
    }

    /// Marks the workspace archived, recording `session` (if any) as stopped with the
    /// workspace's diff at that moment, when known.
    pub fn archive(
        &self,
        id: WorkspaceId,
        session: Option<SessionId>,
        changes: Option<DiffStat>,
    ) -> Result<()> {
        self.store
            .update_workspace_status(id, WorkspaceStatus::Archived)
            .map_err(WorkspaceError::Archive)?;
        if let Some(session) = session {
            self.store
                .finish_session(session, &stopped("workspace archived"), changes)
                .map_err(WorkspaceError::StopSession)?;
        }
        Ok(())
    }

    pub fn restore(&self, id: WorkspaceId) -> Result<()> {
        self.store
            .update_workspace_status(id, WorkspaceStatus::Active)
            .map_err(WorkspaceError::Restore)
    }

    /// Removes the workspace's worktree, prunes, optionally deletes its branch, then deletes its
    /// rows. Only the worktree path recorded in the store is touched, and only if the repository
    /// lists it as a linked worktree.
    pub fn delete(&self, id: WorkspaceId, options: DeleteOptions) -> Result<DeleteOutcome> {
        let workspace = self.store.get_workspace(id)?;
        let project = self.store.get_project(workspace.project_id)?;
        let repo = &project.repo_path;
        let path = &workspace.worktree_path;

        if path.exists() {
            let wanted = canonical(path);
            let listed = self
                .git
                .worktrees(repo)
                .map_err(|source| WorkspaceError::ListWorktrees {
                    repo: repo.clone(),
                    source,
                })?
                .into_iter()
                .find(|wt| canonical(&wt.path) == wanted);
            match listed {
                Some(wt) if !wt.is_main && !wt.bare => {
                    self.git
                        .remove_worktree(repo, path, options.force)
                        .map_err(|source| WorkspaceError::RemoveWorktree {
                            path: path.clone(),
                            source,
                        })?;
                }
                Some(_) => return Err(WorkspaceError::MainWorktree(path.clone())),
                None => {
                    return Err(WorkspaceError::NotAWorktree {
                        path: path.clone(),
                        repo: repo.clone(),
                    });
                }
            }
        }
        if let Err(err) = self.git.prune_worktrees(repo) {
            tracing::warn!(%err, "git worktree prune failed");
        }

        let mut outcome = DeleteOutcome::default();
        if options.delete_branch {
            if !workspace.branch.starts_with(BRANCH_PREFIX) {
                outcome.branch_error = Some(format!(
                    "kept branch `{}`: not created by Hangar",
                    workspace.branch
                ));
            } else if let Err(err) = self.git.delete_branch(repo, &workspace.branch, false) {
                outcome.branch_error = Some(format!("kept branch `{}`: {err}", workspace.branch));
            }
        }
        self.store
            .delete_workspace(id)
            .map_err(WorkspaceError::DeleteRow)?;
        Ok(outcome)
    }

    /// Forgets the project (and its archived workspaces) without touching the repository or
    /// any worktree on disk. Refused while it has active workspaces.
    pub fn remove_project(&self, id: ProjectId) -> Result<()> {
        let active: Vec<String> = self
            .store
            .list_workspaces(id)?
            .into_iter()
            .filter(|w| w.status == WorkspaceStatus::Active)
            .map(|w| w.name)
            .collect();
        if !active.is_empty() {
            return Err(WorkspaceError::ActiveWorkspaces(active));
        }
        self.store
            .delete_project(id)
            .map_err(WorkspaceError::RemoveProject)
    }
}

/// Reads the worktree's status for the delete confirmation.
pub fn delete_summary(git: &dyn VersionControl, workspace: &Workspace) -> Result<DeleteSummary> {
    let path = &workspace.worktree_path;
    if !path.exists() {
        return Ok(DeleteSummary::default());
    }
    let counts = git
        .change_counts(path)
        .map_err(|source| WorkspaceError::Status {
            path: path.clone(),
            source,
        })?;
    let ahead = git
        .commits_ahead(path, &workspace.base)
        .map_err(|err| tracing::warn!(%err, "failed to count commits ahead of base"))
        .ok();
    Ok(DeleteSummary {
        exists: true,
        uncommitted: counts.uncommitted,
        untracked: counts.untracked,
        ahead,
    })
}

fn canonical(path: &Path) -> PathBuf {
    path.canonicalize().unwrap_or_else(|_| path.to_path_buf())
}

#[cfg(test)]
mod tests {
    use hangar_core::ports::ListedWorktree;

    use super::*;
    use crate::fakes::{FakeGit, FakeStore};

    fn project(store: &FakeStore) -> Project {
        let project = Project::new("My App", PathBuf::from("/repo"), "main");
        store.insert_project(&project).unwrap();
        project
    }

    fn root() -> PathBuf {
        PathBuf::from("/nonexistent/hangar-usecases-tests")
    }

    #[test]
    fn create_names_branch_and_worktree_from_the_slug() {
        let (store, git) = (
            FakeStore::default(),
            FakeGit::with_branches(&["hangar/fix-login"]),
        );
        let project = project(&store);
        let ws = Workspaces::new(&store, &git)
            .create(&project, &root(), &NewWorkspace::named(" Fix login "))
            .unwrap();
        assert_eq!(
            (ws.name.as_str(), ws.slug.as_str()),
            ("Fix login", "fix-login-2")
        );
        assert_eq!(ws.branch, "hangar/fix-login-2");
        assert_eq!(ws.worktree_path, root().join("my-app/fix-login-2"));
        assert_eq!(git.calls(), ["add hangar/fix-login-2 from main"]);
        assert_eq!(store.list_workspaces(project.id).unwrap(), [ws]);
    }

    #[test]
    fn archive_stops_the_session_with_the_workspace_diff() {
        let (store, git) = (FakeStore::default(), FakeGit::default());
        let project = project(&store);
        let workspaces = Workspaces::new(&store, &git);
        let ws = workspaces
            .create(&project, &root(), &NewWorkspace::named("Docs"))
            .unwrap();
        let session = hangar_core::Session::new(ws.id, hangar_core::AgentKind::Claude);
        store.insert_session(&session).unwrap();
        let stat = DiffStat {
            files: 1,
            insertions: 2,
            deletions: 3,
        };
        workspaces
            .archive(ws.id, Some(session.id), Some(stat))
            .unwrap();
        assert_eq!(
            store.get_workspace(ws.id).unwrap().status,
            WorkspaceStatus::Archived
        );
        let recorded = store.list_sessions(ws.id).unwrap().remove(0);
        assert!(crate::sessions::is_stopped(&recorded.status));
        assert_eq!(recorded.changes, Some(stat));
    }

    #[test]
    fn create_rolls_back_branch_and_worktree_when_the_row_cannot_be_saved() {
        let store = FakeStore {
            refuse_workspaces: true,
            ..FakeStore::default()
        };
        let git = FakeGit::default();
        let project = project(&store);
        let err = Workspaces::new(&store, &git)
            .create(
                &project,
                &root(),
                &NewWorkspace {
                    base: Some("develop".into()),
                    ..NewWorkspace::named("Fix")
                },
            )
            .unwrap_err();
        assert!(matches!(err, WorkspaceError::SaveWorkspace(_)), "{err}");
        let path = root().join("my-app/fix");
        assert_eq!(
            git.calls(),
            [
                "add hangar/fix from develop".to_owned(),
                format!("remove {} force=true", path.display()),
                "delete-branch hangar/fix force=true".to_owned(),
            ]
        );
        assert!(!git.branch_exists(&project.repo_path, "hangar/fix").unwrap());
    }

    #[test]
    fn create_refuses_bad_or_taken_branch_names_before_touching_git() {
        let (store, git) = (FakeStore::default(), FakeGit::with_branches(&["taken"]));
        let project = project(&store);
        let workspaces = Workspaces::new(&store, &git);
        let err = workspaces
            .create(
                &project,
                &root(),
                &NewWorkspace {
                    branch: Some("bad name".into()),
                    ..NewWorkspace::named("x")
                },
            )
            .unwrap_err();
        assert_eq!(err.to_string(), "`bad name` is not a valid branch name");
        let err = workspaces
            .create(
                &project,
                &root(),
                &NewWorkspace {
                    branch: Some("taken".into()),
                    ..NewWorkspace::named("x")
                },
            )
            .unwrap_err();
        assert_eq!(err.to_string(), "branch `taken` already exists");
        let err = workspaces
            .create(&project, &root(), &NewWorkspace::named("  "))
            .unwrap_err();
        assert_eq!(err.to_string(), "workspace name is empty");
        assert!(git.calls().is_empty());
    }

    #[test]
    fn delete_refuses_the_main_worktree() {
        let main = std::env::temp_dir();
        let git = FakeGit {
            listed: vec![ListedWorktree {
                path: main.clone(),
                is_main: true,
                bare: false,
            }],
            ..FakeGit::default()
        };
        let store = FakeStore::default();
        let project = project(&store);
        let ws = Workspace {
            id: WorkspaceId::new(),
            project_id: project.id,
            name: "main".into(),
            slug: "main".into(),
            branch: "main".into(),
            base: "main".into(),
            worktree_path: main.clone(),
            status: WorkspaceStatus::Active,
            created_at: Timestamp::now(),
            task: None,
            group: None,
        };
        store.insert_workspace(&ws).unwrap();
        let err = Workspaces::new(&store, &git)
            .delete(ws.id, DeleteOptions::default())
            .unwrap_err();
        assert!(matches!(err, WorkspaceError::MainWorktree(_)), "{err}");
        assert!(git.calls().is_empty());
        assert_eq!(store.list_workspaces(project.id).unwrap().len(), 1);
    }

    #[test]
    fn remove_project_lists_the_active_workspaces() {
        let (store, git) = (FakeStore::default(), FakeGit::default());
        let project = project(&store);
        let workspaces = Workspaces::new(&store, &git);
        workspaces
            .create(&project, &root(), &NewWorkspace::named("One"))
            .unwrap();
        let err = workspaces.remove_project(project.id).unwrap_err();
        assert_eq!(
            err.to_string(),
            "archive or delete its workspaces first: One"
        );
    }
}
