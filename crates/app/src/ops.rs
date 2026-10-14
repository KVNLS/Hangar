//! Blocking operations on the store, Git and agents. Every function here does disk or process
//! I/O: call them from a background executor, never from the UI thread.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{Context as _, anyhow};
use hangar_agents::{AgentAdapter as _, AgentHandle, PtyAdapter, SpawnContext, UpdateHook};
use hangar_core::{
    Project, ProjectId, Session, SessionId, SessionStatus, Store, Timestamp, Workspace,
    WorkspaceId, WorkspaceStatus, slugify,
};
use hangar_terminal::ExitInfo;

use crate::agents;
use crate::paths::{BRANCH_PREFIX, branch_name, unique_slug, worktree_path};
use crate::state::{ChangesLayout, PaneLayout, ProjectEntry};

pub fn open_store(path: &Path) -> anyhow::Result<Arc<Store>> {
    let store =
        Store::open(path).with_context(|| format!("failed to open database {}", path.display()))?;
    Ok(Arc::new(store))
}

/// Setting key of the saved [`PaneLayout`].
pub const LAYOUT_SETTING: &str = "layout";
/// Setting key of the agent picked for new workspaces.
pub const AGENT_SETTING: &str = "agent";
/// Setting key of the saved [`ChangesLayout`].
pub const CHANGES_SETTING: &str = "changes_panel";

/// Everything the window needs at startup.
pub struct Loaded {
    pub entries: Vec<ProjectEntry>,
    pub layout: Option<PaneLayout>,
    pub agent: Option<String>,
    pub changes: Option<ChangesLayout>,
    /// Saved settings that could not be read; the defaults are used instead.
    pub settings_error: Option<anyhow::Error>,
}

/// Loads every project with its workspaces (active and archived) and the saved settings.
/// Sessions left `Starting`/`Running` by a previous run are marked failed: their processes
/// ended with that run.
pub fn load(store: &Store) -> anyhow::Result<Loaded> {
    let mut entries = Vec::new();
    for project in store.list_projects()? {
        let workspaces = store.list_workspaces(project.id)?;
        for ws in &workspaces {
            for session in store.list_sessions(ws.id)? {
                if !session.status.is_terminal() {
                    store.update_session_status(
                        session.id,
                        &SessionStatus::Failed {
                            message: "Hangar quit while the session was running".into(),
                        },
                    )?;
                }
            }
        }
        entries.push(ProjectEntry {
            project,
            workspaces,
        });
    }
    let settings = (|| {
        let layout = store.get_setting::<PaneLayout>(LAYOUT_SETTING)?;
        let agent = store.get_setting::<String>(AGENT_SETTING)?;
        let changes = store.get_setting::<ChangesLayout>(CHANGES_SETTING)?;
        anyhow::Ok((layout, agent, changes))
    })()
    .context("failed to read saved layout");
    let (layout, agent, changes, settings_error) = match settings {
        Ok((layout, agent, changes)) => (layout, agent, changes, None),
        Err(err) => (None, None, None, Some(err)),
    };
    Ok(Loaded {
        entries,
        layout,
        agent,
        changes,
        settings_error,
    })
}

pub fn save_layout(store: &Store, layout: &PaneLayout) -> anyhow::Result<()> {
    store
        .set_setting(LAYOUT_SETTING, layout)
        .context("failed to save pane layout")
}

pub fn save_changes(store: &Store, changes: &ChangesLayout) -> anyhow::Result<()> {
    store
        .set_setting(CHANGES_SETTING, changes)
        .context("failed to save the changes panel layout")
}

pub fn save_agent(store: &Store, agent: &str) -> anyhow::Result<()> {
    store
        .set_setting(AGENT_SETTING, agent)
        .context("failed to save the selected agent")
}

/// Registers the repository containing `path`, or returns the existing project for it.
pub fn add_project(store: &Store, path: &Path) -> anyhow::Result<Project> {
    let root = hangar_worktree::repo_root(path)
        .with_context(|| format!("{} is not inside a Git repository", path.display()))?;
    if let Some(existing) = store
        .list_projects()?
        .into_iter()
        .find(|p| p.repo_path == root)
    {
        return Ok(existing);
    }
    let base = hangar_worktree::default_branch(&root)
        .with_context(|| format!("failed to read branches of {}", root.display()))?
        .unwrap_or_else(|| "HEAD".to_owned());
    let name = root
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| root.display().to_string());
    let project = Project::new(name, root, base);
    store.insert_project(&project)?;
    Ok(project)
}

/// Creates the branch, the worktree and the workspace row. The slug gets a numeric suffix when
/// the name, branch or directory is already taken.
pub fn create_workspace(
    store: &Store,
    project: &Project,
    name: &str,
    worktrees_root: &Path,
) -> anyhow::Result<Workspace> {
    let name = name.trim();
    if name.is_empty() {
        return Err(anyhow!("workspace name is empty"));
    }
    let repo = &project.repo_path;
    let project_slug = slugify(&project.name);
    let existing: Vec<String> = store
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
        match hangar_worktree::branch_exists(repo, &branch_name(candidate)) {
            Ok(taken) => taken,
            Err(err) => {
                git_error.get_or_insert(err);
                false
            }
        }
    });
    if let Some(err) = git_error {
        return Err(err).context("failed to check existing branches");
    }

    let branch = branch_name(&slug);
    let path = worktree_path(worktrees_root, &project_slug, &slug);
    let options = hangar_worktree::CreateOptions {
        branch: branch.clone(),
        base: Some(project.default_base.clone()),
        path: path.clone(),
        reuse_existing_branch: false,
    };
    let worktree = hangar_worktree::create(repo, &options)
        .with_context(|| format!("failed to create worktree {}", path.display()))?;

    let workspace = Workspace {
        id: WorkspaceId::new(),
        project_id: project.id,
        name: name.to_owned(),
        slug,
        branch,
        base: project.default_base.clone(),
        worktree_path: worktree.path,
        status: WorkspaceStatus::Active,
        created_at: Timestamp::now(),
    };
    if let Err(err) = store.insert_workspace(&workspace) {
        rollback_worktree(repo, &path, &workspace.branch);
        return Err(err).context("failed to save workspace");
    }
    Ok(workspace)
}

fn rollback_worktree(repo: &Path, path: &Path, branch: &str) {
    if let Err(err) = hangar_worktree::remove(repo, path, true) {
        tracing::warn!(%err, "rollback: failed to remove worktree");
    }
    if let Err(err) = hangar_worktree::delete_branch(repo, branch, true) {
        tracing::warn!(%err, "rollback: failed to delete branch");
    }
}

pub struct StartedSession {
    pub session: Session,
    pub handle: AgentHandle,
    /// The agent runs but its session row could not be written.
    pub record_error: Option<anyhow::Error>,
}

/// Spawns `adapter` in the workspace and records the session. A failed spawn is recorded too.
pub fn start_session(
    store: &Store,
    adapter: &PtyAdapter,
    workspace: &Workspace,
    path_env: Option<&str>,
    on_update: UpdateHook,
) -> anyhow::Result<StartedSession> {
    let mut ctx = SpawnContext::new(&workspace.worktree_path);
    if let Some(path) = path_env {
        ctx.env.push(("PATH".to_owned(), path.to_owned()));
    }
    ctx.on_update = Some(on_update);

    let mut session = Session::new(workspace.id, agents::kind(&adapter.id));
    session.args = adapter.args.clone();
    match adapter.spawn(&ctx) {
        Ok(handle) => {
            session.command = handle.program().display().to_string();
            session.status = SessionStatus::Running;
            let record_error = store
                .insert_session(&session)
                .context("the agent started but its session could not be saved")
                .err();
            Ok(StartedSession {
                session,
                handle,
                record_error,
            })
        }
        Err(err) => {
            session.status = SessionStatus::Failed {
                message: err.to_string(),
            };
            session.ended_at = Some(Timestamp::now());
            let err = anyhow::Error::new(err)
                .context(format!("failed to start {}", adapter.display_name));
            match store.insert_session(&session) {
                Ok(()) => Err(err),
                Err(record) => {
                    Err(err.context(format!("the failure could not be saved: {record}")))
                }
            }
        }
    }
}

pub fn finish_session(
    store: &Store,
    id: SessionId,
    status: &SessionStatus,
) -> anyhow::Result<Session> {
    Ok(store.update_session_status(id, status)?)
}

/// Records that a running session was stopped by Hangar (archive, delete).
pub fn stop_session(store: &Store, id: SessionId, reason: &str) -> anyhow::Result<()> {
    store
        .update_session_status(
            id,
            &SessionStatus::Failed {
                message: format!("stopped: {reason}"),
            },
        )
        .context("failed to record the stopped session")?;
    Ok(())
}

/// Marks the workspace archived, recording `session` (if any) as stopped.
pub fn archive_workspace(
    store: &Store,
    id: WorkspaceId,
    session: Option<SessionId>,
) -> anyhow::Result<()> {
    store
        .update_workspace_status(id, WorkspaceStatus::Archived)
        .context("failed to archive the workspace")?;
    if let Some(session) = session {
        stop_session(store, session, "workspace archived")?;
    }
    Ok(())
}

pub fn restore_workspace(store: &Store, id: WorkspaceId) -> anyhow::Result<()> {
    store
        .update_workspace_status(id, WorkspaceStatus::Active)
        .context("failed to restore the workspace")
}

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

/// Reads the worktree's status for the delete confirmation.
pub fn delete_summary(workspace: &Workspace) -> anyhow::Result<DeleteSummary> {
    let path = &workspace.worktree_path;
    if !path.exists() {
        return Ok(DeleteSummary::default());
    }
    let status = hangar_worktree::status(path)
        .with_context(|| format!("failed to read the status of {}", path.display()))?;
    let uncommitted = status
        .changes
        .iter()
        .filter(|c| c.kind != hangar_worktree::ChangeKind::Untracked)
        .count();
    let ahead = hangar_worktree::Git::new(path)
        .run(["rev-list", "--count", &format!("{}..HEAD", workspace.base)])
        .map_err(|err| tracing::warn!(%err, "failed to count commits ahead of base"))
        .ok()
        .and_then(|out| out.trim().parse().ok());
    Ok(DeleteSummary {
        exists: true,
        uncommitted,
        untracked: status.untracked,
        ahead,
    })
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

/// Removes the workspace's worktree, prunes, optionally deletes its branch, then deletes its
/// rows. Only the worktree path recorded in the store is touched, and only if git lists it as
/// a linked worktree of the project's repository.
pub fn delete_workspace(
    store: &Store,
    id: WorkspaceId,
    options: DeleteOptions,
) -> anyhow::Result<DeleteOutcome> {
    let workspace = store.get_workspace(id)?;
    let project = store.get_project(workspace.project_id)?;
    let repo = &project.repo_path;
    let path = &workspace.worktree_path;

    if path.exists() {
        let wanted = canonical(path);
        let listed = hangar_worktree::list(repo)
            .with_context(|| format!("failed to list worktrees of {}", repo.display()))?
            .into_iter()
            .find(|wt| canonical(&wt.path) == wanted);
        match listed {
            Some(wt) if !wt.is_main && !wt.bare => {
                hangar_worktree::remove(repo, path, options.force)
                    .with_context(|| format!("failed to remove worktree {}", path.display()))?;
            }
            Some(_) => {
                return Err(anyhow!(
                    "refusing to delete {}: it is the repository's main working tree",
                    path.display()
                ));
            }
            None => {
                return Err(anyhow!(
                    "refusing to delete {}: it is not a worktree of {}",
                    path.display(),
                    repo.display()
                ));
            }
        }
    }
    if let Err(err) = hangar_worktree::prune(repo) {
        tracing::warn!(%err, "git worktree prune failed");
    }

    let mut outcome = DeleteOutcome::default();
    if options.delete_branch {
        if !workspace.branch.starts_with(BRANCH_PREFIX) {
            outcome.branch_error = Some(format!(
                "kept branch `{}`: not created by Hangar",
                workspace.branch
            ));
        } else if let Err(err) = hangar_worktree::delete_branch(repo, &workspace.branch, false) {
            outcome.branch_error = Some(format!("kept branch `{}`: {err}", workspace.branch));
        }
    }
    store
        .delete_workspace(id)
        .context("the worktree was removed but the workspace could not be deleted")?;
    Ok(outcome)
}

/// Forgets the project (and its archived workspaces) without touching the repository or any
/// worktree on disk. Refused while it has active workspaces.
pub fn remove_project(store: &Store, id: ProjectId) -> anyhow::Result<()> {
    let active: Vec<String> = store
        .list_workspaces(id)?
        .into_iter()
        .filter(|w| w.status == WorkspaceStatus::Active)
        .map(|w| w.name)
        .collect();
    if !active.is_empty() {
        return Err(anyhow!(
            "archive or delete its workspaces first: {}",
            active.join(", ")
        ));
    }
    store
        .delete_project(id)
        .context("failed to remove project")?;
    Ok(())
}

/// Session status for a finished program.
pub fn exit_status(exit: &Result<ExitInfo, String>) -> SessionStatus {
    match exit {
        Ok(ExitInfo {
            signal: Some(signal),
            ..
        }) => SessionStatus::Failed {
            message: format!("killed by {signal}"),
        },
        Ok(info) => SessionStatus::Exited {
            code: i32::try_from(info.code).unwrap_or(i32::MAX),
        },
        Err(message) => SessionStatus::Failed {
            message: message.clone(),
        },
    }
}

/// The repository root for a debug/automation path, canonicalised for comparisons.
pub fn canonical(path: &Path) -> PathBuf {
    path.canonicalize().unwrap_or_else(|_| path.to_path_buf())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agents;
    use hangar_core::{SessionStatus, Store};
    use hangar_terminal::ExitInfo;
    use std::path::Path;
    use std::process::Command;
    use std::sync::Arc;
    use std::time::Duration;

    fn git(dir: &Path, args: &[&str]) {
        let status = Command::new("git")
            .args(args)
            .current_dir(dir)
            .status()
            .unwrap();
        assert!(status.success(), "git {args:?}");
    }

    fn repo() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        git(dir.path(), &["init", "-q", "-b", "main"]);
        git(
            dir.path(),
            &[
                "-c",
                "user.name=t",
                "-c",
                "user.email=t@t",
                "commit",
                "-q",
                "--allow-empty",
                "-m",
                "init",
            ],
        );
        dir
    }

    #[test]
    fn add_project_resolves_root_and_is_idempotent() {
        let repo = repo();
        let sub = repo.path().join("sub");
        std::fs::create_dir(&sub).unwrap();
        let store = Store::open_in_memory().unwrap();
        let project = add_project(&store, &sub).unwrap();
        assert_eq!(
            project.repo_path.canonicalize().unwrap(),
            repo.path().canonicalize().unwrap()
        );
        assert_eq!(project.default_base, "main");
        assert_eq!(add_project(&store, repo.path()).unwrap().id, project.id);
        assert_eq!(store.list_projects().unwrap().len(), 1);
    }

    #[test]
    fn add_project_rejects_non_repo() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open_in_memory().unwrap();
        assert!(add_project(&store, dir.path()).is_err());
    }

    #[test]
    fn create_workspace_makes_branch_worktree_and_row() {
        let repo = repo();
        let root = tempfile::tempdir().unwrap();
        let store = Store::open_in_memory().unwrap();
        let project = add_project(&store, repo.path()).unwrap();

        let ws = create_workspace(&store, &project, "Fix Login", root.path()).unwrap();
        assert_eq!(ws.slug, "fix-login");
        assert_eq!(ws.branch, "hangar/fix-login");
        assert!(ws.worktree_path.is_dir());
        assert!(
            ws.worktree_path
                .ends_with(format!("{}/fix-login", hangar_core::slugify(&project.name)))
        );

        let again = create_workspace(&store, &project, "fix login", root.path()).unwrap();
        assert_eq!(again.slug, "fix-login-2");
        assert_eq!(again.branch, "hangar/fix-login-2");

        let loaded = load(&store).unwrap();
        assert_eq!(loaded.entries.len(), 1);
        assert_eq!(loaded.entries[0].workspaces.len(), 2);

        assert!(create_workspace(&store, &project, "   ", root.path()).is_err());
    }

    #[test]
    fn session_runs_records_and_stale_sessions_are_failed_on_load() {
        let repo = repo();
        let root = tempfile::tempdir().unwrap();
        let store = Store::open_in_memory().unwrap();
        let project = add_project(&store, repo.path()).unwrap();
        let ws = create_workspace(&store, &project, "run", root.path()).unwrap();

        let adapter =
            hangar_agents::PtyAdapter::new("sh", "sh", "/bin/sh").with_args(["-c", "pwd; exit 3"]);
        let started = start_session(&store, &adapter, &ws, None, Arc::new(|| {})).unwrap();
        let exit = started
            .handle
            .session()
            .wait_for_exit(Duration::from_secs(10))
            .unwrap();
        assert_eq!(exit_status(&exit), SessionStatus::Exited { code: 3 });
        let row = store.get_session(started.session.id).unwrap();
        assert_eq!(row.status, SessionStatus::Running);

        load(&store).unwrap();
        let row = store.get_session(started.session.id).unwrap();
        assert!(matches!(row.status, SessionStatus::Failed { .. }));
        assert!(row.ended_at.is_some());
    }

    #[test]
    fn failed_spawn_is_recorded() {
        let repo = repo();
        let root = tempfile::tempdir().unwrap();
        let store = Store::open_in_memory().unwrap();
        let project = add_project(&store, repo.path()).unwrap();
        let ws = create_workspace(&store, &project, "x", root.path()).unwrap();
        let adapter = agents::preset("claude").unwrap();
        assert!(
            start_session(&store, &adapter, &ws, Some("/nonexistent"), Arc::new(|| {})).is_err()
        );
        let sessions = store.list_sessions(ws.id).unwrap();
        assert_eq!(sessions.len(), 1);
        assert!(matches!(sessions[0].status, SessionStatus::Failed { .. }));
    }

    #[test]
    fn exit_status_mapping() {
        let ok = |code, signal: Option<&str>| {
            Ok(ExitInfo {
                code,
                signal: signal.map(str::to_owned),
            })
        };
        assert_eq!(exit_status(&ok(0, None)), SessionStatus::Exited { code: 0 });
        assert!(matches!(
            exit_status(&ok(1, Some("SIGKILL"))),
            SessionStatus::Failed { .. }
        ));
        assert!(matches!(
            exit_status(&Err("boom".into())),
            SessionStatus::Failed { .. }
        ));
    }

    fn commit_all(dir: &Path, message: &str) {
        git(dir, &["add", "-A"]);
        git(
            dir,
            &[
                "-c",
                "user.name=t",
                "-c",
                "user.email=t@t",
                "commit",
                "-q",
                "-m",
                message,
            ],
        );
    }

    fn branch_exists(repo: &Path, branch: &str) -> bool {
        hangar_worktree::branch_exists(repo, branch).unwrap()
    }

    struct Fixture {
        repo: tempfile::TempDir,
        root: tempfile::TempDir,
        store: Store,
        project: Project,
    }

    fn fixture() -> Fixture {
        let repo = repo();
        let root = tempfile::tempdir().unwrap();
        let store = Store::open_in_memory().unwrap();
        let project = add_project(&store, repo.path()).unwrap();
        Fixture {
            repo,
            root,
            store,
            project,
        }
    }

    impl Fixture {
        fn workspace(&self, name: &str) -> Workspace {
            let ws = create_workspace(&self.store, &self.project, name, self.root.path()).unwrap();
            self.store
                .insert_session(&Session::new(ws.id, agents::kind("shell")))
                .unwrap();
            ws
        }

        fn assert_rows_gone(&self, ws: &Workspace) {
            assert!(self.store.get_workspace(ws.id).is_err());
            assert!(self.store.list_sessions(ws.id).unwrap().is_empty());
        }
    }

    #[test]
    fn delete_clean_workspace_keeps_branch_by_default() {
        let f = fixture();
        let ws = f.workspace("clean");
        let summary = delete_summary(&ws).unwrap();
        assert_eq!(
            summary,
            DeleteSummary {
                exists: true,
                uncommitted: 0,
                untracked: 0,
                ahead: Some(0),
            }
        );

        let outcome = delete_workspace(&f.store, ws.id, DeleteOptions::default()).unwrap();
        assert!(outcome.branch_error.is_none());
        assert!(!ws.worktree_path.exists());
        assert!(branch_exists(f.repo.path(), &ws.branch));
        f.assert_rows_gone(&ws);
        // The repository and the worktree root are untouched.
        assert!(f.repo.path().join(".git").is_dir());
        assert!(f.root.path().is_dir());
        assert!(
            hangar_worktree::list(f.repo.path())
                .unwrap()
                .iter()
                .all(|wt| wt.is_main)
        );
    }

    #[test]
    fn delete_can_delete_a_merged_branch() {
        let f = fixture();
        let ws = f.workspace("merged");
        let options = DeleteOptions {
            force: false,
            delete_branch: true,
        };
        let outcome = delete_workspace(&f.store, ws.id, options).unwrap();
        assert!(outcome.branch_error.is_none());
        assert!(!ws.worktree_path.exists());
        assert!(!branch_exists(f.repo.path(), &ws.branch));
        f.assert_rows_gone(&ws);
    }

    #[test]
    fn delete_keeps_an_unmerged_branch_and_reports_why() {
        let f = fixture();
        let ws = f.workspace("work");
        std::fs::write(ws.worktree_path.join("new.txt"), "x").unwrap();
        commit_all(&ws.worktree_path, "work");
        assert_eq!(delete_summary(&ws).unwrap().ahead, Some(1));

        let options = DeleteOptions {
            force: false,
            delete_branch: true,
        };
        let outcome = delete_workspace(&f.store, ws.id, options).unwrap();
        let error = outcome.branch_error.unwrap();
        assert!(error.contains("hangar/work"), "{error}");
        assert!(branch_exists(f.repo.path(), &ws.branch));
        assert!(!ws.worktree_path.exists());
        f.assert_rows_gone(&ws);
    }

    #[test]
    fn dirty_workspace_needs_force() {
        let f = fixture();
        let ws = f.workspace("dirty");
        std::fs::write(ws.worktree_path.join("scratch.txt"), "x").unwrap();
        let summary = delete_summary(&ws).unwrap();
        assert_eq!((summary.uncommitted, summary.untracked), (0, 1));
        assert!(summary.is_dirty());

        assert!(delete_workspace(&f.store, ws.id, DeleteOptions::default()).is_err());
        assert!(ws.worktree_path.join("scratch.txt").exists());
        assert!(f.store.get_workspace(ws.id).is_ok());

        let force = DeleteOptions {
            force: true,
            delete_branch: false,
        };
        delete_workspace(&f.store, ws.id, force).unwrap();
        assert!(!ws.worktree_path.exists());
        f.assert_rows_gone(&ws);
    }

    #[test]
    fn delete_of_missing_directory_only_prunes_and_forgets() {
        let f = fixture();
        let ws = f.workspace("gone");
        std::fs::remove_dir_all(&ws.worktree_path).unwrap();
        assert_eq!(delete_summary(&ws).unwrap(), DeleteSummary::default());
        delete_workspace(&f.store, ws.id, DeleteOptions::default()).unwrap();
        f.assert_rows_gone(&ws);
        assert_eq!(hangar_worktree::list(f.repo.path()).unwrap().len(), 1);
    }

    #[test]
    fn delete_refuses_paths_that_are_not_linked_worktrees() {
        let f = fixture();
        let outside = tempfile::tempdir().unwrap();
        std::fs::write(outside.path().join("keep.txt"), "x").unwrap();
        for (slug, path) in [
            ("main", f.repo.path().to_path_buf()),
            ("outside", outside.path().to_path_buf()),
        ] {
            let ws = Workspace {
                id: WorkspaceId::new(),
                project_id: f.project.id,
                name: slug.into(),
                slug: slug.into(),
                branch: branch_name(slug),
                base: "main".into(),
                worktree_path: path.clone(),
                status: WorkspaceStatus::Active,
                created_at: Timestamp::now(),
            };
            f.store.insert_workspace(&ws).unwrap();
            let force = DeleteOptions {
                force: true,
                delete_branch: true,
            };
            let err = delete_workspace(&f.store, ws.id, force).unwrap_err();
            assert!(format!("{err:#}").contains("refusing"), "{err:#}");
            assert!(f.store.get_workspace(ws.id).is_ok());
        }
        assert!(outside.path().join("keep.txt").exists());
        assert!(f.repo.path().join(".git").is_dir());
    }

    #[test]
    fn archive_restore_and_load_keep_archived_workspaces() {
        let f = fixture();
        let ws = f.workspace("later");
        let session = f.store.list_sessions(ws.id).unwrap()[0].id;
        archive_workspace(&f.store, ws.id, Some(session)).unwrap();
        assert_eq!(
            f.store.get_workspace(ws.id).unwrap().status,
            WorkspaceStatus::Archived
        );
        let row = f.store.get_session(session).unwrap();
        assert!(matches!(row.status, SessionStatus::Failed { .. }));
        assert!(ws.worktree_path.is_dir());

        let loaded = load(&f.store).unwrap();
        assert_eq!(loaded.entries[0].archived().count(), 1);
        assert_eq!(loaded.entries[0].active().count(), 0);

        restore_workspace(&f.store, ws.id).unwrap();
        let loaded = load(&f.store).unwrap();
        assert_eq!(loaded.entries[0].active().count(), 1);
    }

    #[test]
    fn remove_project_requires_no_active_workspaces() {
        let f = fixture();
        let ws = f.workspace("busy");
        let err = remove_project(&f.store, f.project.id).unwrap_err();
        assert!(err.to_string().contains("busy"), "{err}");

        archive_workspace(&f.store, ws.id, None).unwrap();
        remove_project(&f.store, f.project.id).unwrap();
        assert!(f.store.list_projects().unwrap().is_empty());
        // The repository and the archived worktree stay on disk.
        assert!(f.repo.path().join(".git").is_dir());
        assert!(ws.worktree_path.is_dir());
    }

    #[test]
    fn settings_are_loaded_and_bad_ones_reported() {
        let f = fixture();
        let loaded = load(&f.store).unwrap();
        assert!(loaded.layout.is_none() && loaded.agent.is_none());
        assert!(loaded.settings_error.is_none());

        let layout = PaneLayout {
            panes: vec![Some(WorkspaceId::new()), None],
            focused: 1,
            split: Some(0.4),
        };
        save_layout(&f.store, &layout).unwrap();
        save_agent(&f.store, "codex").unwrap();
        let loaded = load(&f.store).unwrap();
        assert_eq!(loaded.layout, Some(layout));
        assert_eq!(loaded.agent.as_deref(), Some("codex"));
        assert!(loaded.changes.is_none());

        let changes = ChangesLayout {
            open: true,
            width: 640.0,
        };
        save_changes(&f.store, &changes).unwrap();
        assert_eq!(load(&f.store).unwrap().changes, Some(changes));

        f.store.set_setting(LAYOUT_SETTING, &42).unwrap();
        let loaded = load(&f.store).unwrap();
        assert!(loaded.layout.is_none());
        assert!(loaded.settings_error.is_some());
        assert_eq!(loaded.entries.len(), 1);
    }

    #[test]
    fn delete_summary_description() {
        let clean = DeleteSummary {
            exists: true,
            ahead: Some(0),
            ..Default::default()
        };
        assert_eq!(
            clean.describe("main"),
            [
                "No uncommitted or untracked files.",
                "No commits ahead of main."
            ]
        );
        let dirty = DeleteSummary {
            exists: true,
            uncommitted: 2,
            untracked: 1,
            ahead: Some(3),
        };
        assert_eq!(
            dirty.describe("main"),
            [
                "2 uncommitted and 1 untracked file(s) will be lost.",
                "3 commit(s) ahead of main."
            ]
        );
        assert_eq!(DeleteSummary::default().describe("main").len(), 1);
    }
}
