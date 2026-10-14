//! Blocking operations on the store, Git and agents. Every function here does disk or process
//! I/O: call them from a background executor, never from the UI thread.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::mpsc::Receiver;

use anyhow::{Context as _, anyhow};
use hangar_agents::acp::{AcpAdapter, AcpEvent, AcpSession};
use hangar_agents::{AgentAdapter as _, AgentHandle, PtyAdapter, SpawnContext, UpdateHook};
use hangar_core::{
    Project, ProjectId, Session, SessionId, SessionStatus, Timestamp, Workspace, WorkspaceId,
};
use hangar_store_sqlite::Store;
use hangar_terminal::ExitInfo;
use hangar_usecases::workspaces::Workspaces;
pub use hangar_usecases::workspaces::{DeleteOptions, DeleteOutcome, DeleteSummary};
use hangar_worktree::GitCli;

use crate::agents::{self, AgentExtras};
use crate::setting_keys;
use crate::state::{ChangesLayout, DeviceLayout, PaneLayout, ProjectEntry};
use crate::ui::theme::Appearance;

pub fn open_store(path: &Path) -> anyhow::Result<Arc<Store>> {
    let store =
        Store::open(path).with_context(|| format!("failed to open database {}", path.display()))?;
    Ok(Arc::new(store))
}

/// Local models served by Ollama, offered to OpenCode as an extra provider.
#[derive(Clone, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct OllamaSettings {
    pub enabled: bool,
    /// As typed; empty means `hangar_agents::ollama::DEFAULT_HOST`.
    pub host: String,
    pub default_model: Option<String>,
}

/// Everything the window needs at startup.
pub struct Loaded {
    pub entries: Vec<ProjectEntry>,
    /// Structured conversations workspaces can resume.
    pub chats: HashMap<WorkspaceId, SavedChat>,
    pub layout: Option<PaneLayout>,
    pub agent: Option<String>,
    pub changes: Option<ChangesLayout>,
    pub device: Option<DeviceLayout>,
    pub appearance: Option<Appearance>,
    pub ollama: Option<OllamaSettings>,
    /// Saved settings that could not be read; the defaults are used instead.
    pub settings_error: Option<anyhow::Error>,
}

/// The ACP conversation a workspace can resume, saved under [`setting_keys::chat`].
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct SavedChat {
    /// The preset that ran it; only that preset resumes it.
    pub agent: String,
    pub session: String,
}

/// `(config id, value)` pairs in the order they were chosen.
pub fn saved_agent_config(store: &Store, agent: &str) -> Vec<(String, String)> {
    store
        .get_setting(&setting_keys::agent_config(agent))
        .unwrap_or_else(|err| {
            tracing::warn!(agent, "cannot read saved agent settings: {err}");
            None
        })
        .unwrap_or_default()
}

pub fn remember_agent_config(
    store: &Store,
    agent: &str,
    config_id: &str,
    value: &str,
) -> anyhow::Result<()> {
    let mut saved = saved_agent_config(store, agent);
    saved.retain(|(id, _)| id != config_id);
    saved.push((config_id.to_owned(), value.to_owned()));
    Ok(store.set_setting(&setting_keys::agent_config(agent), &saved)?)
}

/// Loads every project with its workspaces (active and archived) and the saved settings.
/// Sessions left `Starting`/`Running` by a previous run are marked failed: their processes
/// ended with that run.
pub fn load(store: &Store) -> anyhow::Result<Loaded> {
    let mut entries = Vec::new();
    let mut chats = HashMap::new();
    for project in store.list_projects()? {
        let workspaces = store.list_workspaces(project.id)?;
        for ws in &workspaces {
            match store.get_setting(&setting_keys::chat(ws.id)) {
                Ok(Some(chat)) => {
                    chats.insert(ws.id, chat);
                }
                Ok(None) => {}
                Err(err) => {
                    tracing::warn!(workspace = %ws.id, %err, "saved conversation unreadable")
                }
            }
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
        let layout = store.get_setting(&setting_keys::LAYOUT)?;
        let agent = store.get_setting(&setting_keys::AGENT)?;
        let changes = store.get_setting(&setting_keys::CHANGES)?;
        anyhow::Ok((layout, agent, changes))
    })()
    .context("failed to read saved layout");
    let (layout, agent, changes, mut settings_error) = match settings {
        Ok((layout, agent, changes)) => (layout, agent, changes, None),
        Err(err) => (None, None, None, Some(err)),
    };
    // Read on its own so a bad device setting does not reset the pane layout.
    let device = match store
        .get_setting(&setting_keys::DEVICE)
        .context("failed to read the device panel settings")
    {
        Ok(device) => device,
        Err(err) => {
            settings_error.get_or_insert(err);
            None
        }
    };
    let appearance = match store
        .get_setting(&setting_keys::APPEARANCE)
        .context("failed to read the appearance setting")
    {
        Ok(appearance) => appearance,
        Err(err) => {
            settings_error.get_or_insert(err);
            None
        }
    };
    let ollama = match store
        .get_setting(&setting_keys::OLLAMA)
        .context("failed to read the Ollama settings")
    {
        Ok(ollama) => ollama,
        Err(err) => {
            settings_error.get_or_insert(err);
            None
        }
    };
    Ok(Loaded {
        entries,
        chats,
        layout,
        agent,
        changes,
        device,
        appearance,
        ollama,
        settings_error,
    })
}

pub fn save_layout(store: &Store, layout: &PaneLayout) -> anyhow::Result<()> {
    store
        .set_setting(&setting_keys::LAYOUT, layout)
        .context("failed to save pane layout")
}

pub fn save_changes(store: &Store, changes: &ChangesLayout) -> anyhow::Result<()> {
    store
        .set_setting(&setting_keys::CHANGES, changes)
        .context("failed to save the changes panel layout")
}

pub fn save_device(store: &Store, device: &DeviceLayout) -> anyhow::Result<()> {
    store
        .set_setting(&setting_keys::DEVICE, device)
        .context("failed to save the device panel settings")
}

pub fn save_appearance(store: &Store, appearance: Appearance) -> anyhow::Result<()> {
    store
        .set_setting(&setting_keys::APPEARANCE, &appearance)
        .context("failed to save the appearance")
}

pub fn save_ollama(store: &Store, ollama: &OllamaSettings) -> anyhow::Result<()> {
    store
        .set_setting(&setting_keys::OLLAMA, ollama)
        .context("failed to save the Ollama settings")
}

pub fn save_agent(store: &Store, agent: &str) -> anyhow::Result<()> {
    store
        .set_setting(&setting_keys::AGENT, &agent.to_owned())
        .context("failed to save the selected agent")
}

/// Registers the repository containing `path`, or returns the existing project for it.
pub fn add_project(store: &Store, path: &Path) -> anyhow::Result<Project> {
    Ok(workspaces(store).add_project(path)?)
}

/// Creates the branch, the worktree and the workspace row. The slug gets a numeric suffix when
/// the name, branch or directory is already taken.
pub fn create_workspace(
    store: &Store,
    project: &Project,
    name: &str,
    worktrees_root: &Path,
) -> anyhow::Result<Workspace> {
    create_workspace_from(store, project, name, worktrees_root, None, None)
}

/// As [`create_workspace`], starting from `base` (default: the project's default base) on a
/// branch named `branch` (default: `hangar/<slug>`, suffixed until free). A given branch name is
/// used as is and must not exist yet.
pub fn create_workspace_from(
    store: &Store,
    project: &Project,
    name: &str,
    worktrees_root: &Path,
    base: Option<&str>,
    branch: Option<&str>,
) -> anyhow::Result<Workspace> {
    Ok(workspaces(store).create(project, name, worktrees_root, base, branch)?)
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
    start_session_with(
        store,
        adapter,
        workspace,
        path_env,
        on_update,
        &AgentExtras::default(),
    )
}

/// [`start_session`] with `extra` arguments and environment (appended after the adapter's,
/// winning on conflicts) and files written first (0600, missing directories 0700). Files inside
/// the worktree are refused.
pub fn start_session_with(
    store: &Store,
    adapter: &PtyAdapter,
    workspace: &Workspace,
    path_env: Option<&str>,
    on_update: UpdateHook,
    extra: &AgentExtras,
) -> anyhow::Result<StartedSession> {
    let mut ctx = SpawnContext::new(&workspace.worktree_path);
    if let Some(path) = path_env {
        ctx.env.push(("PATH".to_owned(), path.to_owned()));
    }
    ctx.env.extend(extra.env.iter().cloned());
    ctx.extra_args.extend(extra.args.iter().cloned());
    ctx.on_update = Some(on_update);

    let mut session = Session::new(workspace.id, agents::kind(&adapter.id));
    session.args = adapter.args.iter().chain(&extra.args).cloned().collect();
    let spawned = write_private_files(&workspace.worktree_path, &extra.files)
        .and_then(|()| adapter.spawn(&ctx).map_err(anyhow::Error::new))
        .map(|handle| {
            let command = handle.program().display().to_string();
            (handle, command)
        });
    let (session, handle, record_error) =
        record_start(store, session, &adapter.display_name, spawned)?;
    Ok(StartedSession {
        session,
        handle,
        record_error,
    })
}

/// A structured agent that started and holds an open ACP session.
pub struct StartedAcpSession {
    pub session: Session,
    pub acp: Arc<AcpSession>,
    pub events: Receiver<AcpEvent>,
    /// The agent runs but its session row could not be written.
    pub record_error: Option<anyhow::Error>,
}

/// [`start_session_with`] for an ACP agent: spawns it, runs the handshake (blocking, up to a
/// few minutes) and records the session. `extra.mcp_servers` are given to `session/new`.
pub fn start_acp_session(
    store: &Store,
    adapter: &AcpAdapter,
    workspace: &Workspace,
    path_env: Option<&str>,
    on_update: UpdateHook,
    extra: &AgentExtras,
    resume: Option<&str>,
) -> anyhow::Result<StartedAcpSession> {
    let mut ctx = SpawnContext::new(&workspace.worktree_path);
    if let Some(path) = path_env {
        ctx.env.push(("PATH".to_owned(), path.to_owned()));
    }
    ctx.env.extend(extra.env.iter().cloned());
    ctx.extra_args.extend(extra.args.iter().cloned());
    ctx.on_update = Some(on_update);

    let mut session = Session::new(workspace.id, agents::kind(&adapter.id));
    session.args = adapter.args.iter().chain(&extra.args).cloned().collect();
    let wanted = saved_agent_config(store, &adapter.id);
    let spawned = write_private_files(&workspace.worktree_path, &extra.files)
        .and_then(|()| {
            adapter
                .start(&ctx, &extra.mcp_servers, resume)
                .map_err(anyhow::Error::new)
        })
        .map(|(mut acp, events)| {
            for refused in acp.apply_config(&wanted) {
                tracing::warn!(agent = %adapter.id, "saved setting not applied: {refused}");
            }
            let command = acp.program().display().to_string();
            ((acp, events), command)
        });
    let (session, (acp, events), record_error) =
        record_start(store, session, &adapter.display_name, spawned)?;
    Ok(StartedAcpSession {
        session,
        acp: Arc::new(acp),
        events,
        record_error,
    })
}

/// Records the outcome of starting an agent: `Running` with its command, or `Failed`.
fn record_start<T>(
    store: &Store,
    mut session: Session,
    display_name: &str,
    spawned: anyhow::Result<(T, String)>,
) -> anyhow::Result<(Session, T, Option<anyhow::Error>)> {
    match spawned {
        Ok((agent, command)) => {
            session.command = command;
            session.status = SessionStatus::Running;
            let record_error = store
                .insert_session(&session)
                .context("the agent started but its session could not be saved")
                .err();
            Ok((session, agent, record_error))
        }
        Err(err) => {
            session.status = SessionStatus::Failed {
                message: format!("{err:#}"),
            };
            session.ended_at = Some(Timestamp::now());
            let err = err.context(format!("failed to start {display_name}"));
            match store.insert_session(&session) {
                Ok(()) => Err(err),
                Err(record) => {
                    Err(err.context(format!("the failure could not be saved: {record}")))
                }
            }
        }
    }
}

/// Writes each file with mode 0600, creating missing directories with mode 0700.
fn write_private_files(worktree: &Path, files: &[(PathBuf, String)]) -> anyhow::Result<()> {
    use std::io::Write as _;
    use std::os::unix::fs::{DirBuilderExt as _, OpenOptionsExt as _, PermissionsExt as _};
    for (path, contents) in files {
        if !path.is_absolute() || path.starts_with(worktree) {
            return Err(anyhow!(
                "refusing to write {} (agent files must be absolute and outside the worktree)",
                path.display()
            ));
        }
        if let Some(dir) = path.parent() {
            std::fs::DirBuilder::new()
                .recursive(true)
                .mode(0o700)
                .create(dir)
                .with_context(|| format!("cannot create {}", dir.display()))?;
        }
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(path)
            .with_context(|| format!("cannot write {}", path.display()))?;
        file.set_permissions(std::fs::Permissions::from_mode(0o600))
            .with_context(|| format!("cannot restrict {}", path.display()))?;
        file.write_all(contents.as_bytes())
            .with_context(|| format!("cannot write {}", path.display()))?;
    }
    Ok(())
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
    Ok(workspaces(store).stop_session(id, reason)?)
}

/// Marks the workspace archived, recording `session` (if any) as stopped.
pub fn archive_workspace(
    store: &Store,
    id: WorkspaceId,
    session: Option<SessionId>,
) -> anyhow::Result<()> {
    Ok(workspaces(store).archive(id, session)?)
}

pub fn restore_workspace(store: &Store, id: WorkspaceId) -> anyhow::Result<()> {
    Ok(workspaces(store).restore(id)?)
}

/// Reads the worktree's status for the delete confirmation.
pub fn delete_summary(workspace: &Workspace) -> anyhow::Result<DeleteSummary> {
    Ok(hangar_usecases::workspaces::delete_summary(
        &GitCli, workspace,
    )?)
}

/// Removes the workspace's worktree, prunes, optionally deletes its branch, then deletes its
/// rows. Only the worktree path recorded in the store is touched, and only if git lists it as
/// a linked worktree of the project's repository.
pub fn delete_workspace(
    store: &Store,
    id: WorkspaceId,
    options: DeleteOptions,
) -> anyhow::Result<DeleteOutcome> {
    Ok(workspaces(store).delete(id, options)?)
}

/// Forgets the project (and its archived workspaces) without touching the repository or any
/// worktree on disk. Refused while it has active workspaces.
pub fn remove_project(store: &Store, id: ProjectId) -> anyhow::Result<()> {
    Ok(workspaces(store).remove_project(id)?)
}

fn workspaces(store: &Store) -> Workspaces<'_> {
    Workspaces::new(store, &GitCli)
}

/// Session status for a finished program.
/// How an ACP agent's process ended: its exit code, or `Failed` when a signal ended it.
pub fn acp_exit_status(code: Option<i32>) -> SessionStatus {
    match code {
        Some(code) => SessionStatus::Exited { code },
        None => SessionStatus::Failed {
            message: "stopped by a signal".to_owned(),
        },
    }
}

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
    use hangar_core::{SessionStatus, Setting, WorkspaceStatus, branch_name};
    use hangar_store_sqlite::Store;
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
    fn create_workspace_from_a_base_on_a_chosen_branch() {
        let repo = repo();
        let root = tempfile::tempdir().unwrap();
        let store = Store::open_in_memory().unwrap();
        let project = add_project(&store, repo.path()).unwrap();
        let git = |args: &[&str]| {
            let out = std::process::Command::new("git")
                .arg("-C")
                .arg(repo.path())
                .args(args)
                .output()
                .unwrap();
            assert!(out.status.success(), "git {args:?}: {out:?}");
            String::from_utf8_lossy(&out.stdout).trim().to_owned()
        };
        git(&["branch", "release"]);
        let release = git(&["rev-parse", "release"]);

        let ws = create_workspace_from(
            &store,
            &project,
            "Hotfix",
            root.path(),
            Some("release"),
            Some("fix/crash"),
        )
        .unwrap();
        assert_eq!(
            (ws.slug.as_str(), ws.branch.as_str()),
            ("hotfix", "fix/crash")
        );
        assert_eq!(ws.base, "release");
        assert_eq!(git(&["rev-parse", "fix/crash"]), release);

        let taken = create_workspace_from(
            &store,
            &project,
            "again",
            root.path(),
            None,
            Some("fix/crash"),
        );
        assert!(taken.unwrap_err().to_string().contains("already exists"));
        let bad = create_workspace_from(&store, &project, "x", root.path(), None, Some("a..b"));
        assert!(
            bad.unwrap_err()
                .to_string()
                .contains("not a valid branch name")
        );
        // Blank fields fall back to the defaults.
        let ws = create_workspace_from(&store, &project, "plain", root.path(), Some(" "), Some(""))
            .unwrap();
        assert_eq!(ws.branch, "hangar/plain");
        assert_eq!(ws.base, project.default_base);
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
    fn saved_conversations_load_per_workspace() {
        let repo = repo();
        let root = tempfile::tempdir().unwrap();
        let store = Store::open_in_memory().unwrap();
        let project = add_project(&store, repo.path()).unwrap();
        let ws = create_workspace(&store, &project, "chat", root.path()).unwrap();
        let chat = SavedChat {
            agent: "claude-acp".into(),
            session: "s-1".into(),
        };
        store
            .set_setting(&setting_keys::chat(ws.id), &chat)
            .unwrap();
        let loaded = load(&store).unwrap();
        assert_eq!(loaded.chats.get(&ws.id), Some(&chat));
    }

    #[test]
    fn agent_config_choices_replace_and_keep_order() {
        let store = Store::open_in_memory().unwrap();
        assert!(saved_agent_config(&store, "codex-acp").is_empty());
        remember_agent_config(&store, "codex-acp", "model", "gpt-5.4").unwrap();
        remember_agent_config(&store, "codex-acp", "reasoning_effort", "high").unwrap();
        remember_agent_config(&store, "codex-acp", "model", "gpt-5.5").unwrap();
        remember_agent_config(&store, "claude-acp", "model", "opus").unwrap();
        let pair = |a: &str, b: &str| (a.to_owned(), b.to_owned());
        assert_eq!(
            saved_agent_config(&store, "codex-acp"),
            [pair("reasoning_effort", "high"), pair("model", "gpt-5.5")]
        );
        assert_eq!(
            saved_agent_config(&store, "claude-acp"),
            [pair("model", "opus")]
        );
    }

    #[test]
    fn failed_spawn_is_recorded() {
        let repo = repo();
        let root = tempfile::tempdir().unwrap();
        let store = Store::open_in_memory().unwrap();
        let project = add_project(&store, repo.path()).unwrap();
        let ws = create_workspace(&store, &project, "x", root.path()).unwrap();
        let Some(agents::AgentPreset::Pty(adapter)) = agents::preset("claude") else {
            panic!("claude is a terminal preset");
        };
        assert!(
            start_session(&store, &adapter, &ws, Some("/nonexistent"), Arc::new(|| {})).is_err()
        );
        let sessions = store.list_sessions(ws.id).unwrap();
        assert_eq!(sessions.len(), 1);
        assert!(matches!(sessions[0].status, SessionStatus::Failed { .. }));
    }

    #[test]
    fn session_extras_write_files_and_reach_the_agent() {
        use std::os::unix::fs::PermissionsExt as _;
        let repo = repo();
        let root = tempfile::tempdir().unwrap();
        let data = tempfile::tempdir().unwrap();
        let store = Store::open_in_memory().unwrap();
        let project = add_project(&store, repo.path()).unwrap();
        let ws = create_workspace(&store, &project, "extras", root.path()).unwrap();

        let config = data.path().join("mcp/ws.json");
        let out = data.path().join("out.txt");
        let adapter = hangar_agents::PtyAdapter::new("sh", "sh", "/bin/sh").with_args([
            "-c",
            r#"printf '%s|%s|%s' "$HANGAR_X" "$1" "$(cat "$CONFIG")" > "$OUT""#,
            "sh",
        ]);
        let extra = AgentExtras {
            mcp_servers: Vec::new(),
            args: vec!["--flag".into()],
            env: vec![
                ("HANGAR_X".into(), "x1".into()),
                ("CONFIG".into(), config.display().to_string()),
                ("OUT".into(), out.display().to_string()),
            ],
            files: vec![(config.clone(), "{}".into())],
        };
        let started =
            start_session_with(&store, &adapter, &ws, None, Arc::new(|| {}), &extra).unwrap();
        let exit = started
            .handle
            .session()
            .wait_for_exit(Duration::from_secs(10))
            .unwrap();
        assert_eq!(exit_status(&exit), SessionStatus::Exited { code: 0 });
        assert_eq!(std::fs::read_to_string(&out).unwrap(), "x1|--flag|{}");
        let mode = |p: &Path| std::fs::metadata(p).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode(&config), 0o600);
        assert_eq!(mode(config.parent().unwrap()), 0o700);
        assert_eq!(
            started.session.args.last().map(String::as_str),
            Some("--flag")
        );

        let inside = AgentExtras {
            files: vec![(ws.worktree_path.join(".mcp.json"), "{}".into())],
            ..AgentExtras::default()
        };
        let err = start_session_with(&store, &adapter, &ws, None, Arc::new(|| {}), &inside)
            .err()
            .unwrap();
        assert!(
            format!("{err:#}").contains("outside the worktree"),
            "{err:#}"
        );
        assert!(!ws.worktree_path.join(".mcp.json").exists());
        let sessions = store.list_sessions(ws.id).unwrap();
        assert!(
            sessions
                .iter()
                .any(|s| matches!(s.status, SessionStatus::Failed { .. }))
        );
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
            sizes: vec![0.4, 0.6],
            split: None,
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

        f.store.set_setting(&Setting::new("layout"), &42).unwrap();
        let loaded = load(&f.store).unwrap();
        assert!(loaded.layout.is_none());
        assert!(loaded.settings_error.is_some());
        assert_eq!(loaded.entries.len(), 1);
    }

    #[test]
    fn device_settings_round_trip() {
        use crate::device_panel::{DeviceKey, Platform};

        let f = fixture();
        assert!(load(&f.store).unwrap().device.is_none());
        let device = DeviceLayout {
            open: true,
            width: 380.0,
            selected: Some(DeviceKey {
                platform: Platform::Ios,
                id: "E33FE595-67AD-470C-AD94-A38EAC9D0F38".into(),
            }),
            stats: true,
            logs: true,
            log_level: Some("error".into()),
        };
        save_device(&f.store, &device).unwrap();
        let loaded = load(&f.store).unwrap();
        assert_eq!(loaded.device, Some(device.clone()));
        assert!(loaded.settings_error.is_none());

        let android = DeviceLayout {
            selected: Some(DeviceKey {
                platform: Platform::Android,
                id: "emulator-5554".into(),
            }),
            ..device
        };
        save_device(&f.store, &android).unwrap();
        assert_eq!(load(&f.store).unwrap().device, Some(android));

        // A value saved before Android existed (platform written as a plain string).
        #[derive(serde::Serialize)]
        struct Saved {
            open: bool,
            width: f32,
            selected: SavedKey,
            stats: bool,
        }
        #[derive(serde::Serialize)]
        struct SavedKey {
            platform: &'static str,
            id: &'static str,
        }
        f.store
            .set_setting(
                &Setting::new("device_panel"),
                &Saved {
                    open: true,
                    width: 400.0,
                    selected: SavedKey {
                        platform: "ios",
                        id: "U",
                    },
                    stats: false,
                },
            )
            .unwrap();
        let old = load(&f.store).unwrap().device.unwrap();
        assert_eq!(
            old.selected,
            Some(DeviceKey {
                platform: Platform::Ios,
                id: "U".into()
            })
        );

        // Fields added later default when an older value is read.
        #[derive(serde::Serialize)]
        struct Old {
            open: bool,
            width: f32,
        }
        f.store
            .set_setting(
                &Setting::new("device_panel"),
                &Old {
                    open: false,
                    width: 300.0,
                },
            )
            .unwrap();
        let old = load(&f.store).unwrap().device.unwrap();
        assert_eq!(
            (old.open, old.width, old.selected, old.stats),
            (false, 300.0, None, false)
        );
        assert!(!old.logs && old.log_level.is_none());

        // A broken device setting is reported but leaves the other settings alone.
        save_agent(&f.store, "codex").unwrap();
        f.store
            .set_setting(&Setting::new("device_panel"), &"nope")
            .unwrap();
        let loaded = load(&f.store).unwrap();
        assert!(loaded.device.is_none());
        assert!(loaded.settings_error.is_some());
        assert_eq!(loaded.agent.as_deref(), Some("codex"));
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
