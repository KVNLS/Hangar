//! App side of the MCP tools: resolves bridge sessions to workspaces and turns each tool call
//! into an [`McpRequest`] for the GPUI side, which owns the workspace devices and agents.
//!
//! Tool calls run on `hangar_mcp`'s worker threads (one per message) and block there until the
//! GPUI side replies or the request times out; nothing here runs on the UI thread.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use crate::adapters::devices::LogEntry;
use anyhow::Context as _;
use hangar_core::device::{Frame, Input, LogLevel, Platform};
use hangar_core::{AgentSignal, WorkspaceId};
use hangar_mcp::{
    Answer, ChildWorkspace, CreatedWorkspace, DeviceStatus, Sharing, Tool, ToolError, ToolHost,
    ToolOutput, TranscriptEntry, new_session_secret,
};
use tokio::sync::{mpsc, oneshot};

mod dispatch;
#[cfg(test)]
mod fixtures;
mod notices;
mod sharing;

pub use notices::Notices;
pub use sharing::{acquisition, agent_sharing, holds, released, still_queued};

pub type Reply<T> = oneshot::Sender<anyhow::Result<T>>;

/// The latest frame of a workspace's device with its screen size in points, as currently
/// oriented (the space of tap/swipe coordinates).
#[derive(Debug, Clone)]
pub struct ScreenFrame {
    pub frame: Frame,
    pub width_pt: f64,
    pub height_pt: f64,
}

/// One device operation for the GPUI side. Every request targets the device of `ws`; reply
/// with `Err` carrying a `crate::adapters::devices::Error::Unsupported` (or a [`ToolError`]) for things
/// the device cannot do, any other error for failures. Dropping `reply` fails the call.
#[derive(Debug)]
pub enum McpRequest {
    Status {
        ws: WorkspaceId,
        reply: Reply<DeviceStatus>,
    },
    /// Opens/boots the device first if needed.
    Frame {
        ws: WorkspaceId,
        reply: Reply<ScreenFrame>,
    },
    Input {
        ws: WorkspaceId,
        input: Input,
        reply: Reply<()>,
    },
    Text {
        ws: WorkspaceId,
        text: String,
        reply: Reply<()>,
    },
    /// The most recent `lines` entries (oldest first) at or above `min_level`, from `process`.
    Logs {
        ws: WorkspaceId,
        lines: usize,
        min_level: Option<LogLevel>,
        process: Option<String>,
        reply: Reply<Vec<LogEntry>>,
    },
    /// Same as ⌘R. Replies once the app is launched (a short summary) or with an error carrying
    /// the last build lines.
    BuildAndRun {
        ws: WorkspaceId,
        reply: Reply<String>,
    },
    Launch {
        ws: WorkspaceId,
        app_id: String,
        reply: Reply<()>,
    },
    /// `device_acquire`: reserves the workspace's device, or keeps its place in line and waits
    /// up to `wait` for its turn (touching the reservation while it waits). Never boots. A
    /// `platform` other than the attached one is switched to first.
    Acquire {
        ws: WorkspaceId,
        wait: Duration,
        platform: Option<Platform>,
        reply: Reply<Acquisition>,
    },
    /// `device_release`: ends the workspace's reservation.
    Release {
        ws: WorkspaceId,
        reply: Reply<Released>,
    },
    /// `ask_user`: shows the question and replies with the user's answer.
    Ask {
        ws: WorkspaceId,
        question: String,
        choices: Vec<String>,
        reply: Reply<Answer>,
    },
    /// What the workspace's terminal agent reported through its hooks (no reply), with its
    /// transcript file when the hook named an acceptable one.
    Signal {
        ws: WorkspaceId,
        signal: AgentSignal,
        transcript: Option<PathBuf>,
    },
    /// The workspaces `ws`'s agent created (orchestration tools, like the three below).
    Children {
        ws: WorkspaceId,
        reply: Reply<Vec<ChildWorkspace>>,
    },
    /// Creates a child of `ws` and starts (or queues) its agent with `task` as first input.
    CreateChild {
        ws: WorkspaceId,
        task: String,
        name: Option<String>,
        agent: Option<String>,
        reply: Reply<CreatedWorkspace>,
    },
    /// Sends `text` to the agent of `ws`'s child `child` (id or name). Replies with its name.
    PromptChild {
        ws: WorkspaceId,
        child: String,
        text: String,
        reply: Reply<String>,
    },
    /// The last `last` messages of `ws`'s child's structured agent, with the child's name.
    Transcript {
        ws: WorkspaceId,
        child: String,
        last: usize,
        reply: Reply<(String, Vec<TranscriptEntry>)>,
    },
}

impl McpRequest {
    pub fn workspace(&self) -> WorkspaceId {
        match self {
            Self::Status { ws, .. }
            | Self::Frame { ws, .. }
            | Self::Input { ws, .. }
            | Self::Text { ws, .. }
            | Self::Logs { ws, .. }
            | Self::BuildAndRun { ws, .. }
            | Self::Launch { ws, .. }
            | Self::Acquire { ws, .. }
            | Self::Release { ws, .. }
            | Self::Signal { ws, .. }
            | Self::Children { ws, .. }
            | Self::CreateChild { ws, .. }
            | Self::PromptChild { ws, .. }
            | Self::Transcript { ws, .. }
            | Self::Ask { ws, .. } => *ws,
        }
    }
}

/// What `device_acquire` got.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Acquisition {
    /// The workspace holds the device (else it waits in line).
    pub granted: bool,
    pub sharing: Sharing,
}

/// What `device_release` did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Released {
    /// Another workspace was waiting for the device.
    pub had_waiters: bool,
    /// The workspace next in line.
    pub next: Option<String>,
    /// The workspace was itself waiting for the device: its waiting calls were cancelled.
    pub left_line: bool,
}

/// `raw` when it is a `.jsonl` file inside `root` (Claude Code's projects directory) once links
/// are resolved: an agent must not make Hangar read any other file on a lead's behalf.
pub fn accept_transcript(raw: &Path, root: &Path) -> Option<PathBuf> {
    let path = raw.canonicalize().ok()?;
    let root = root.canonicalize().ok()?;
    (path.starts_with(&root) && path.extension().is_some_and(|e| e == "jsonl") && path.is_file())
        .then_some(path)
}

/// How long a tool call waits for the GPUI side.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Timeouts {
    pub call: Duration,
    pub build_and_run: Duration,
    /// `workspace_create`: a worktree checkout and the agent's start.
    pub create: Duration,
    /// `ask_user`: how long a question waits for the user.
    pub ask: Duration,
}

/// Kept from a call's timeout for the work after the device is granted, so that a call that
/// waits in line ends with where it stands rather than a bare timeout.
const ACQUIRE_MARGIN: Duration = Duration::from_secs(3);

impl Timeouts {
    /// How long a device tool may wait in line for the device before it reports its place in
    /// line ([`hangar_mcp::queued_message`]).
    pub fn acquire_budget(&self) -> Duration {
        if self.call > ACQUIRE_MARGIN * 2 {
            self.call - ACQUIRE_MARGIN
        } else {
            self.call / 2
        }
    }
}

impl Default for Timeouts {
    fn default() -> Self {
        Self {
            call: Duration::from_secs(30),
            build_and_run: Duration::from_secs(20 * 60),
            create: Duration::from_secs(3 * 60),
            ask: Duration::from_secs(30 * 60),
        }
    }
}

/// Session secret → workspace. Secrets live in memory only.
#[derive(Clone, Default)]
pub struct Sessions(Arc<Mutex<HashMap<String, WorkspaceId>>>);

impl Sessions {
    fn lock(&self) -> MutexGuard<'_, HashMap<String, WorkspaceId>> {
        self.0.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// The workspace's secret, created on first use.
    pub fn register(&self, ws: WorkspaceId) -> String {
        let mut map = self.lock();
        if let Some(secret) = map.iter().find(|(_, w)| **w == ws).map(|(s, _)| s.clone()) {
            return secret;
        }
        let secret = new_session_secret();
        map.insert(secret.clone(), ws);
        secret
    }

    pub fn unregister(&self, ws: WorkspaceId) {
        self.lock().retain(|_, w| *w != ws);
    }

    /// Compares `secret` against every registered one in constant time per entry.
    pub fn resolve(&self, secret: &str) -> Option<WorkspaceId> {
        let map = self.lock();
        let mut found = None;
        for (known, ws) in map.iter() {
            if ct_eq(known.as_bytes(), secret.as_bytes()) {
                found = Some(*ws);
            }
        }
        found
    }

    fn clear(&self) {
        self.lock().clear();
    }
}

fn ct_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

/// Implements the MCP tools by forwarding them to the GPUI side.
pub struct McpHost {
    sessions: Sessions,
    requests: mpsc::UnboundedSender<McpRequest>,
    timeouts: Timeouts,
    /// "Let agents create and drive workspaces", set from the GPUI side.
    orchestration: Arc<AtomicBool>,
    notices: Notices,
    // Only drives the reply timeouts: worker threads block on it, never on the UI thread.
    timer: tokio::runtime::Runtime,
}

impl McpHost {
    pub fn new(
        sessions: Sessions,
        requests: mpsc::UnboundedSender<McpRequest>,
        timeouts: Timeouts,
        orchestration: Arc<AtomicBool>,
        notices: Notices,
    ) -> anyhow::Result<Self> {
        let timer = tokio::runtime::Builder::new_current_thread()
            .enable_time()
            .build()
            .context("cannot start the MCP timer runtime")?;
        Ok(Self {
            sessions,
            requests,
            timeouts,
            orchestration,
            notices,
            timer,
        })
    }

    fn ask<T>(
        &self,
        what: &str,
        timeout: Duration,
        make: impl FnOnce(Reply<T>) -> McpRequest,
    ) -> Result<T, ToolError> {
        let (tx, rx) = oneshot::channel();
        self.requests.send(make(tx)).map_err(|_| {
            ToolError::Failed("Hangar is shutting down; the device is no longer available".into())
        })?;
        match self
            .timer
            .block_on(async { tokio::time::timeout(timeout, rx).await })
        {
            Err(_) => Err(ToolError::Failed(format!(
                "{what} timed out after {}",
                human(timeout)
            ))),
            Ok(Err(_)) => Err(ToolError::Failed(format!(
                "{what} was abandoned by Hangar (the workspace or its device closed)"
            ))),
            Ok(Ok(Err(err))) => Err(tool_error(&err)),
            Ok(Ok(Ok(value))) => Ok(value),
        }
    }
}

impl ToolHost for McpHost {
    fn knows_session(&self, session: &str) -> bool {
        self.sessions.resolve(session).is_some()
    }

    fn orchestration_enabled(&self, session: &str) -> bool {
        self.orchestration.load(Ordering::Relaxed) && self.knows_session(session)
    }

    fn agent_signal(
        &self,
        session: &str,
        signal: AgentSignal,
        transcript: Option<&str>,
    ) -> Result<(), ToolError> {
        let ws = self
            .sessions
            .resolve(session)
            .ok_or(ToolError::UnknownSession)?;
        let transcript = transcript.and_then(|raw| {
            let root =
                std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".claude/projects"))?;
            accept_transcript(Path::new(raw), &root)
        });
        self.requests
            .send(McpRequest::Signal {
                ws,
                signal,
                transcript,
            })
            .map_err(|_| ToolError::Failed("Hangar is shutting down".into()))
    }

    fn call(&self, session: &str, tool: Tool) -> Result<ToolOutput, ToolError> {
        let ws = self
            .sessions
            .resolve(session)
            .ok_or(ToolError::UnknownSession)?;
        if Tool::is_orchestration_name(tool.name()) && !self.orchestration.load(Ordering::Relaxed) {
            return Err(ToolError::Failed(
                "orchestration tools are off in Hangar's settings".into(),
            ));
        }
        if !notices::carries_notices(&tool) {
            return self.run(ws, tool);
        }
        let result = self.run(ws, tool);
        notices::deliver(result, self.notices.take(ws))
    }
}

fn tool_error(err: &anyhow::Error) -> ToolError {
    for cause in err.chain() {
        if let Some(tool) = cause.downcast_ref::<ToolError>() {
            return tool.clone();
        }
        if let Some(crate::adapters::devices::Error::Unsupported(what)) =
            cause.downcast_ref::<crate::adapters::devices::Error>()
        {
            return ToolError::Unsupported((*what).to_owned());
        }
    }
    ToolError::Failed(format!("{err:#}"))
}

fn human(d: Duration) -> String {
    let secs = d.as_secs();
    if secs >= 60 && secs.is_multiple_of(60) {
        format!("{} min", secs / 60)
    } else if secs >= 1 {
        format!("{secs} s")
    } else {
        format!("{} ms", d.as_millis())
    }
}

/// The running MCP listener. Dropping it forgets every session and removes the socket file.
pub struct McpHandle {
    socket: PathBuf,
    sessions: Sessions,
    orchestration: Arc<AtomicBool>,
    notices: Notices,
    timeouts: Timeouts,
}

impl McpHandle {
    /// The workspace's session secret (stable until [`Self::unregister`]); hand it to the agent
    /// as `HANGAR_MCP_SESSION`.
    pub fn register(&self, ws: WorkspaceId) -> String {
        self.sessions.register(ws)
    }

    /// Revokes the workspace's secret: its agent's calls fail with an unknown-session error.
    pub fn unregister(&self, ws: WorkspaceId) {
        self.sessions.unregister(ws);
        self.notices.forget(ws);
    }

    /// Tells the workspace's agent `message` with its next device tool result.
    pub fn notify(&self, ws: WorkspaceId, message: String) {
        self.notices.push(ws, message);
    }

    /// How long a device tool waits in line before it reports its place instead.
    pub fn acquire_budget(&self) -> Duration {
        self.timeouts.acquire_budget()
    }

    /// Lists and allows the orchestration tools for every agent ("Let agents create and drive
    /// workspaces"). Agents that already listed the tools see the change on their next list.
    pub fn set_orchestration(&self, on: bool) {
        self.orchestration.store(on, Ordering::Relaxed);
    }

    /// Hand it to agents as `HANGAR_MCP_SOCKET`.
    pub fn socket_path(&self) -> &Path {
        &self.socket
    }
}

impl Drop for McpHandle {
    fn drop(&mut self) {
        self.sessions.clear();
        self.notices.clear();
        if let Err(e) = std::fs::remove_file(&self.socket)
            && e.kind() != std::io::ErrorKind::NotFound
        {
            tracing::warn!("mcp: cannot remove {}: {e}", self.socket.display());
        }
    }
}

/// Binds `socket` (its directory must already be private, see `paths::serve_on_socket`) and serves
/// MCP on a background thread. The caller consumes the requests on the GPUI side.
pub fn start(socket: &Path) -> anyhow::Result<(McpHandle, mpsc::UnboundedReceiver<McpRequest>)> {
    start_with(socket, Timeouts::default())
}

pub fn start_with(
    socket: &Path,
    timeouts: Timeouts,
) -> anyhow::Result<(McpHandle, mpsc::UnboundedReceiver<McpRequest>)> {
    let listener = hangar_mcp::bind(socket)
        .with_context(|| format!("cannot listen on {}", socket.display()))?;
    let (tx, rx) = mpsc::unbounded_channel();
    let sessions = Sessions::default();
    let orchestration = Arc::new(AtomicBool::new(false));
    let notices = Notices::default();
    let host: Arc<dyn ToolHost> = Arc::new(McpHost::new(
        sessions.clone(),
        tx,
        timeouts,
        Arc::clone(&orchestration),
        notices.clone(),
    )?);
    // The handle owns the socket file from here on, so a failed spawn still removes it.
    let handle = McpHandle {
        socket: socket.to_owned(),
        sessions,
        orchestration,
        notices,
        timeouts,
    };
    std::thread::Builder::new()
        .name("mcp-serve".into())
        .spawn(move || hangar_mcp::serve(listener, host))
        .context("cannot spawn the MCP server thread")?;
    tracing::info!("mcp: listening on {}", socket.display());
    Ok((handle, rx))
}

#[cfg(test)]
mod tests;
