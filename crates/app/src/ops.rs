//! Blocking operations on the store, Git and agents. Every function here does disk or process
//! I/O: call them from a background executor, never from the UI thread.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{Context as _, anyhow};
use hangar_agents::{AgentAdapter as _, AgentHandle, PtyAdapter, SpawnContext, UpdateHook};
use hangar_core::{
    Project, Session, SessionId, SessionStatus, Store, Timestamp, Workspace, WorkspaceId,
    WorkspaceStatus, slugify,
};
use hangar_terminal::ExitInfo;

use crate::agents;
use crate::paths::{branch_name, unique_slug, worktree_path};
use crate::state::ProjectEntry;

pub fn open_store(path: &Path) -> anyhow::Result<Arc<Store>> {
    let store =
        Store::open(path).with_context(|| format!("failed to open database {}", path.display()))?;
    Ok(Arc::new(store))
}

/// Loads every project with its active workspaces. Sessions left `Starting`/`Running` by a
/// previous run are marked failed: their processes ended with that run.
pub fn load(store: &Store) -> anyhow::Result<Vec<ProjectEntry>> {
    let mut entries = Vec::new();
    for project in store.list_projects()? {
        let workspaces = store
            .list_workspaces(project.id)?
            .into_iter()
            .filter(|ws| ws.status == WorkspaceStatus::Active)
            .collect::<Vec<_>>();
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
    Ok(entries)
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
            if let Err(err) = store.insert_session(&session) {
                // The process runs fine; only its record is missing.
                tracing::error!(%err, "failed to record session");
            }
            Ok(StartedSession { session, handle })
        }
        Err(err) => {
            session.status = SessionStatus::Failed {
                message: err.to_string(),
            };
            session.ended_at = Some(Timestamp::now());
            if let Err(err) = store.insert_session(&session) {
                tracing::error!(%err, "failed to record failed session");
            }
            Err(err).with_context(|| format!("failed to start {}", adapter.display_name))
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
    use super::{add_project, create_workspace, exit_status, load, start_session};
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
        assert_eq!(loaded.len(), 1);
        assert_eq!(loaded[0].workspaces.len(), 2);

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
}
