//! Blocking operations on the store, Git and agents. Every function here does disk or process
//! I/O: call them from a background executor, never from the UI thread.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::mpsc::Receiver;

use crate::adapters;
use anyhow::{Context as _, anyhow};
use hangar_agents::acp::approval::ApprovalLevel;
use hangar_agents::acp::protocol::SessionMode;
use hangar_agents::acp::{AcpAdapter, AcpEvent, AcpSession};
use hangar_agents::{AgentAdapter as _, PtyAdapter, SpawnContext, UpdateHook};
use hangar_usecases::agents::StartRequest;

use crate::pty::{self, AgentHandle};
use hangar_core::ports::Store;
use hangar_core::{Project, ProjectId, Session, SessionId, SessionStatus, Workspace, WorkspaceId};
use hangar_terminal::ExitInfo;
use hangar_usecases::sessions::Sessions;
use hangar_usecases::settings::SettingsExt as _;
pub use hangar_usecases::workspaces::NewWorkspace;
use hangar_usecases::workspaces::Workspaces;
pub use hangar_usecases::workspaces::{DeleteOptions, DeleteOutcome, DeleteSummary};

use crate::agents::{self, AgentExtras};
use crate::setting_keys;
use crate::state::{ChangesLayout, DeviceLayout, PaneLayout, ProjectEntry};
use crate::ui::theme::Appearance;

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
    pub notify_attention: Option<bool>,
    /// "Max concurrent agents".
    pub max_agents: Option<u32>,
    /// Starts that were waiting for a slot when Hangar quit.
    pub agent_queue: Vec<(WorkspaceId, StartRequest)>,
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

/// A session mode picked in a workspace's composer, saved under [`setting_keys::workspace_mode`].
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct SavedMode {
    /// Mode ids belong to one agent; another agent ignores it.
    pub agent: String,
    pub mode: String,
}

/// The default approval level; "Ask every time" when unset or unreadable.
pub fn approval_level(store: &dyn Store) -> ApprovalLevel {
    match store.get_setting(&setting_keys::APPROVAL_LEVEL) {
        Ok(Some(id)) => ApprovalLevel::from_id(&id).unwrap_or_else(|| {
            tracing::warn!(id, "unknown approval level saved, asking every time");
            ApprovalLevel::default()
        }),
        Ok(None) => ApprovalLevel::default(),
        Err(err) => {
            tracing::warn!("cannot read the approval level, asking every time: {err}");
            ApprovalLevel::default()
        }
    }
}

pub fn save_approval_level(store: &dyn Store, level: ApprovalLevel) -> anyhow::Result<()> {
    store
        .set_setting(&setting_keys::APPROVAL_LEVEL, &level.id().to_owned())
        .context("failed to save the approval level")
}

pub fn remember_workspace_mode(
    store: &dyn Store,
    workspace: WorkspaceId,
    agent: &str,
    mode: &str,
) -> anyhow::Result<()> {
    let saved = SavedMode {
        agent: agent.to_owned(),
        mode: mode.to_owned(),
    };
    Ok(store.set_setting(&setting_keys::workspace_mode(workspace), &saved)?)
}

/// The mode a new session of `agent` starts in: the workspace's own pick while that agent
/// still offers it, otherwise the level's mode (`None`: the agent keeps its default).
pub fn start_mode<'a>(
    level: ApprovalLevel,
    saved: Option<&SavedMode>,
    agent: &str,
    modes: &'a [SessionMode],
) -> Option<&'a str> {
    saved
        .filter(|saved| saved.agent == agent)
        .and_then(|saved| modes.iter().find(|m| m.id == saved.mode))
        .or_else(|| level.mode(modes))
        .map(|m| m.id.as_str())
}

/// Picks and applies the starting mode of a fresh session (see [`start_mode`]).
fn apply_start_mode(store: &dyn Store, acp: &mut AcpSession, workspace: WorkspaceId) {
    let level = approval_level(store);
    let saved = store
        .get_setting(&setting_keys::workspace_mode(workspace))
        .unwrap_or_else(|err| {
            tracing::warn!(%workspace, "cannot read the workspace's mode: {err}");
            None
        });
    let agent = acp.agent_id().to_owned();
    let Some(mode) = start_mode(level, saved.as_ref(), &agent, acp.modes()).map(str::to_owned)
    else {
        if !acp.modes().is_empty() {
            tracing::info!(
                agent,
                level = level.label(),
                "no session mode fits the approval level; the agent keeps its default"
            );
        }
        return;
    };
    if let Err(err) = acp.apply_mode(&mode) {
        tracing::warn!(agent, mode, "starting mode not applied: {err}");
    }
}

/// `(config id, value)` pairs in the order they were chosen.
pub fn saved_agent_config(store: &dyn Store, agent: &str) -> Vec<(String, String)> {
    store
        .get_setting(&setting_keys::agent_config(agent))
        .unwrap_or_else(|err| {
            tracing::warn!(agent, "cannot read saved agent settings: {err}");
            None
        })
        .unwrap_or_default()
}

pub fn remember_agent_config(
    store: &dyn Store,
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
pub fn load(store: &dyn Store) -> anyhow::Result<Loaded> {
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
    let notify_attention = match store
        .get_setting(&setting_keys::NOTIFY_ATTENTION)
        .context("failed to read the notification setting")
    {
        Ok(on) => on,
        Err(err) => {
            settings_error.get_or_insert(err);
            None
        }
    };
    let max_agents = match store
        .get_setting(&setting_keys::MAX_AGENTS)
        .context("failed to read the max concurrent agents setting")
    {
        Ok(max_agents) => max_agents,
        Err(err) => {
            settings_error.get_or_insert(err);
            None
        }
    };
    let agent_queue = match store
        .get_setting(&setting_keys::AGENT_QUEUE)
        .context("failed to read the queued agent starts")
    {
        Ok(queue) => queue.unwrap_or_default(),
        Err(err) => {
            settings_error.get_or_insert(err);
            Vec::new()
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
        notify_attention,
        max_agents,
        agent_queue,
        settings_error,
    })
}

pub fn save_layout(store: &dyn Store, layout: &PaneLayout) -> anyhow::Result<()> {
    store
        .set_setting(&setting_keys::LAYOUT, layout)
        .context("failed to save pane layout")
}

pub fn save_changes(store: &dyn Store, changes: &ChangesLayout) -> anyhow::Result<()> {
    store
        .set_setting(&setting_keys::CHANGES, changes)
        .context("failed to save the changes panel layout")
}

pub fn save_device(store: &dyn Store, device: &DeviceLayout) -> anyhow::Result<()> {
    store
        .set_setting(&setting_keys::DEVICE, device)
        .context("failed to save the device panel settings")
}

pub fn save_appearance(store: &dyn Store, appearance: Appearance) -> anyhow::Result<()> {
    store
        .set_setting(&setting_keys::APPEARANCE, &appearance)
        .context("failed to save the appearance")
}

pub fn save_ollama(store: &dyn Store, ollama: &OllamaSettings) -> anyhow::Result<()> {
    store
        .set_setting(&setting_keys::OLLAMA, ollama)
        .context("failed to save the Ollama settings")
}

pub fn save_notify_attention(store: &dyn Store, on: bool) -> anyhow::Result<()> {
    store
        .set_setting(&setting_keys::NOTIFY_ATTENTION, &on)
        .context("failed to save the notification setting")
}

pub fn save_max_agents(store: &dyn Store, limit: u32) -> anyhow::Result<()> {
    store
        .set_setting(&setting_keys::MAX_AGENTS, &limit)
        .context("failed to save the max concurrent agents")
}

pub fn save_agent_queue(
    store: &dyn Store,
    queue: &[(WorkspaceId, StartRequest)],
) -> anyhow::Result<()> {
    store
        .set_setting(&setting_keys::AGENT_QUEUE, &queue.to_vec())
        .context("failed to save the queued agent starts")
}

pub fn save_agent(store: &dyn Store, agent: &str) -> anyhow::Result<()> {
    store
        .set_setting(&setting_keys::AGENT, &agent.to_owned())
        .context("failed to save the selected agent")
}

/// Registers the repository containing `path`, or returns the existing project for it.
pub fn add_project(store: &dyn Store, path: &Path) -> anyhow::Result<Project> {
    Ok(workspaces(store).add_project(path)?)
}

/// Creates the branch, the worktree and the workspace row. The slug gets a numeric suffix when
/// the name, branch or directory is already taken.
pub fn create_workspace(
    store: &dyn Store,
    project: &Project,
    name: &str,
    worktrees_root: &Path,
) -> anyhow::Result<Workspace> {
    create_workspace_from(store, project, worktrees_root, &NewWorkspace::named(name))
}

/// As [`create_workspace`], with a base, branch name, task or group (see [`NewWorkspace`]).
pub fn create_workspace_from(
    store: &dyn Store,
    project: &Project,
    worktrees_root: &Path,
    new: &NewWorkspace,
) -> anyhow::Result<Workspace> {
    Ok(workspaces(store).create(project, worktrees_root, new)?)
}

pub struct StartedSession {
    pub session: Session,
    pub handle: AgentHandle,
    /// The agent runs but its session row could not be written.
    pub record_error: Option<anyhow::Error>,
}

/// Spawns `adapter` in the workspace and records the session. A failed spawn is recorded too.
pub fn start_session(
    store: &dyn Store,
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
    store: &dyn Store,
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

    let mut session = Session::new(workspace.id, agents::kind(&adapter.id));
    session.args = adapter.args.iter().chain(&extra.args).cloned().collect();
    let spawned = write_private_files(&workspace.worktree_path, &extra.files)
        .and_then(|()| Ok(adapter.launch(&ctx)?))
        .and_then(|launch| Ok(pty::spawn(launch, on_update)?))
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
    /// The default approval level when it started (read-kind requests follow it).
    pub approval: ApprovalLevel,
    /// The agent runs but its session row could not be written.
    pub record_error: Option<anyhow::Error>,
}

/// [`start_session_with`] for an ACP agent: spawns it, runs the handshake (blocking, up to a
/// few minutes) and records the session. `extra.mcp_servers` are given to `session/new`.
pub fn start_acp_session(
    store: &dyn Store,
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
            apply_start_mode(store, &mut acp, workspace.id);
            let command = acp.program().display().to_string();
            ((acp, events), command)
        });
    let (session, (acp, events), record_error) =
        record_start(store, session, &adapter.display_name, spawned)?;
    Ok(StartedAcpSession {
        session,
        acp: Arc::new(acp),
        events,
        approval: approval_level(store),
        record_error,
    })
}

/// Records the outcome of starting an agent: `Running` with its command, or `Failed`.
fn record_start<T>(
    store: &dyn Store,
    session: Session,
    display_name: &str,
    spawned: anyhow::Result<(T, String)>,
) -> anyhow::Result<(Session, T, Option<anyhow::Error>)> {
    let started = Sessions::new(store).record_start(session, display_name, spawned)?;
    Ok((
        started.session,
        started.agent,
        started.record_error.map(anyhow::Error::new),
    ))
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
    store: &dyn Store,
    id: SessionId,
    status: &SessionStatus,
) -> anyhow::Result<Session> {
    Ok(Sessions::new(store).finish(id, status)?)
}

/// Records that a running session was stopped by Hangar (archive, delete).
pub fn stop_session(store: &dyn Store, id: SessionId, reason: &str) -> anyhow::Result<()> {
    Ok(Sessions::new(store).stop(id, reason)?)
}

/// Marks the workspace archived, recording `session` (if any) as stopped.
pub fn archive_workspace(
    store: &dyn Store,
    id: WorkspaceId,
    session: Option<SessionId>,
) -> anyhow::Result<()> {
    Ok(workspaces(store).archive(id, session)?)
}

pub fn restore_workspace(store: &dyn Store, id: WorkspaceId) -> anyhow::Result<()> {
    Ok(workspaces(store).restore(id)?)
}

/// Reads the worktree's status for the delete confirmation.
pub fn delete_summary(workspace: &Workspace) -> anyhow::Result<DeleteSummary> {
    Ok(hangar_usecases::workspaces::delete_summary(
        adapters::version_control(),
        workspace,
    )?)
}

/// Removes the workspace's worktree, prunes, optionally deletes its branch, then deletes its
/// rows. Only the worktree path recorded in the store is touched, and only if git lists it as
/// a linked worktree of the project's repository.
pub fn delete_workspace(
    store: &dyn Store,
    id: WorkspaceId,
    options: DeleteOptions,
) -> anyhow::Result<DeleteOutcome> {
    Ok(workspaces(store).delete(id, options)?)
}

/// Forgets the project (and its archived workspaces) without touching the repository or any
/// worktree on disk. Refused while it has active workspaces.
pub fn remove_project(store: &dyn Store, id: ProjectId) -> anyhow::Result<()> {
    Ok(workspaces(store).remove_project(id)?)
}

fn workspaces(store: &dyn Store) -> Workspaces<'_> {
    Workspaces::new(store, adapters::version_control())
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
mod tests;
