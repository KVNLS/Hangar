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
    /// The workspace whose agent asked for this one (orchestration tools).
    pub parent: Option<WorkspaceId>,
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
    #[error(
        "{} has no commits yet: make a first commit, then add it",
        .0.display()
    )]
    NoCommits(PathBuf),
    #[error(
        "{} has no main or master branch and HEAD is detached: check out the branch new \
         workspaces should start from, then add it",
        .0.display()
    )]
    NoDefaultBranch(PathBuf),
    #[error(
        "the base branch is `HEAD`, which inside a workspace is the workspace itself: pick a base \
         branch"
    )]
    BaseIsHead,
    #[error("workspace name is empty")]
    EmptyName,
    #[error("`{0}` is not a valid branch name")]
    InvalidBranch(String),
    #[error("branch `{0}` already exists")]
    BranchExists(String),
    /// Git keeps branches as paths: `a` and `a/b` cannot both exist.
    #[error("branch `{branch}` cannot be created next to the existing branch `{existing}`")]
    BranchClash { branch: String, existing: String },
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
    /// Initialized submodules, whose local commits go with the worktree.
    pub submodules: usize,
}

impl DeleteSummary {
    /// Removing the worktree needs `--force` (and loses work).
    pub fn is_dirty(&self) -> bool {
        self.uncommitted > 0 || self.untracked > 0
    }

    /// Removing it needs `--force`: lost files, or submodules git will not remove otherwise.
    pub fn needs_force(&self) -> bool {
        self.is_dirty() || self.submodules > 0
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
        if self.submodules > 0 {
            lines.push(format!(
                "{} initialized submodule(s): their local commits and changes are deleted too.",
                self.submodules
            ));
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
        let branches = |source| WorkspaceError::Branches {
            repo: root.clone(),
            source,
        };
        // An unborn branch is the default of a repository without commits; with no branch at
        // all there is nothing a worktree could start from.
        let local = self.git.local_branches(&root).map_err(branches)?;
        if local.is_empty() {
            return Err(WorkspaceError::NoCommits(root));
        }
        // Never `HEAD`: inside a worktree it is the workspace's own tip.
        let base = match self.git.default_branch(&root).map_err(branches)? {
            Some(base) => base,
            None => match local.as_slice() {
                [only] => only.clone(),
                _ => return Err(WorkspaceError::NoDefaultBranch(root)),
            },
        };
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
        if base == "HEAD" {
            return Err(WorkspaceError::BaseIsHead);
        }
        let chosen = branch.map(str::trim).filter(|b| !b.is_empty());
        if let Some(chosen) = chosen {
            if !self.git.is_valid_branch_name(repo, chosen)? {
                return Err(WorkspaceError::InvalidBranch(chosen.to_owned()));
            }
            if self.git.branch_exists(repo, chosen)? {
                return Err(WorkspaceError::BranchExists(chosen.to_owned()));
            }
        }
        let local = self
            .git
            .local_branches(repo)
            .map_err(WorkspaceError::CheckBranches)?;
        // A branch above the one wanted; for `hangar/<slug>` that is `hangar` itself, which
        // would block every slug.
        let wanted = chosen.map_or_else(|| branch_name(&slugify(name)), str::to_owned);
        if let Some(existing) = local.iter().find(|b| is_parent_branch(b, &wanted)) {
            return Err(WorkspaceError::BranchClash {
                branch: wanted,
                existing: existing.clone(),
            });
        }
        if let Some(chosen) = chosen
            && let Some(existing) = local.iter().find(|b| is_parent_branch(chosen, b))
        {
            return Err(WorkspaceError::BranchClash {
                branch: chosen.to_owned(),
                existing: existing.clone(),
            });
        }
        let project_slug = slugify(&project.name);
        let siblings = self.store.list_workspaces(project.id)?;
        let existing: Vec<String> = siblings.iter().map(|ws| ws.slug.clone()).collect();
        // Rows show names, not slugs: a repeated name gets a number the way its slug does.
        let name = unique_name(name, |n| siblings.iter().any(|ws| ws.name == n));
        let name = name.as_str();
        let slug = unique_slug(&slugify(name), |candidate| {
            if existing.iter().any(|s| s == candidate)
                || worktree_path(worktrees_root, &project_slug, candidate).exists()
            {
                return true;
            }
            if chosen.is_some() {
                return false;
            }
            // Taken, or a branch lives under it (`hangar/dev/sub` blocks `hangar/dev`).
            let branch = branch_name(candidate);
            local
                .iter()
                .any(|b| *b == branch || is_parent_branch(&branch, b))
        });

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
            parent: spec.parent,
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
        submodules: counts.submodules,
    })
}

/// `name`, else `name 2`, `name 3`, … — the first `is_taken` rejects.
fn unique_name(name: &str, is_taken: impl Fn(&str) -> bool) -> String {
    if !is_taken(name) {
        return name.to_owned();
    }
    (2..)
        .map(|n| format!("{name} {n}"))
        .find(|candidate| !is_taken(candidate))
        .unwrap_or_else(|| name.to_owned())
}

/// `parent` is a directory of `child` in git's ref namespace (`a` for `a/b`).
fn is_parent_branch(parent: &str, child: &str) -> bool {
    child
        .strip_prefix(parent)
        .is_some_and(|rest| rest.starts_with('/'))
}

fn canonical(path: &Path) -> PathBuf {
    path.canonicalize().unwrap_or_else(|_| path.to_path_buf())
}

#[cfg(test)]
mod tests;
