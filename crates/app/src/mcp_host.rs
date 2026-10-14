//! App side of the MCP device tools: resolves bridge sessions to workspaces and turns each tool
//! call into an [`McpRequest`] for the GPUI side, which owns the workspace devices.
//!
//! Tool calls run on `hangar_mcp`'s worker threads (one per message) and block there until the
//! GPUI side replies or the request times out; nothing here runs on the UI thread.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use anyhow::Context as _;
use hangar_core::{AgentSignal, WorkspaceId};
use hangar_devices::{Frame, Input, LogEntry, LogLevel};
use hangar_mcp::{
    DeviceStatus, MAX_SIDE, Tool, ToolError, ToolHost, ToolOutput, encode_screenshot,
    new_session_secret,
};
use tokio::sync::{mpsc, oneshot};

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
/// with `Err` carrying a `hangar_devices::Error::Unsupported` (or a [`ToolError`]) for things
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
    /// What the workspace's terminal agent reported through its hooks (no reply).
    Signal {
        ws: WorkspaceId,
        signal: AgentSignal,
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
            | Self::Signal { ws, .. } => *ws,
        }
    }
}

/// How long a tool call waits for the GPUI side.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Timeouts {
    pub call: Duration,
    pub build_and_run: Duration,
}

impl Default for Timeouts {
    fn default() -> Self {
        Self {
            call: Duration::from_secs(30),
            build_and_run: Duration::from_secs(20 * 60),
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

/// Interval between finger moves of a swipe.
const SWIPE_STEP_MS: u64 = 16;
const MAX_SWIPE_MOVES: u64 = 120;

/// A swipe as touch events, each with the delay to wait before sending it: down at `from`,
/// evenly spaced moves over `duration_ms` ending at `to`, then up at `to`.
pub fn swipe_inputs(from: (f64, f64), to: (f64, f64), duration_ms: u64) -> Vec<(Input, Duration)> {
    let moves = (duration_ms / SWIPE_STEP_MS).clamp(1, MAX_SWIPE_MOVES);
    let delay = Duration::from_millis(duration_ms) / moves as u32;
    let mut out = Vec::with_capacity(moves as usize + 2);
    out.push((
        Input::TouchDown {
            x: from.0,
            y: from.1,
        },
        Duration::ZERO,
    ));
    for i in 1..=moves {
        let t = i as f64 / moves as f64;
        out.push((
            Input::TouchMove {
                x: from.0 + (to.0 - from.0) * t,
                y: from.1 + (to.1 - from.1) * t,
            },
            delay,
        ));
    }
    out.push((Input::TouchUp { x: to.0, y: to.1 }, Duration::ZERO));
    out
}

/// Log entries as plain lines for the model.
pub fn format_logs(entries: &[LogEntry]) -> String {
    if entries.is_empty() {
        return "No log lines matched.".to_owned();
    }
    let mut out = String::new();
    for e in entries {
        if !e.timestamp.is_empty() {
            out.push_str(&e.timestamp);
            out.push(' ');
        }
        out.push_str(e.level.as_str());
        let mut source = e.tag.clone();
        if !e.category.is_empty() {
            source = format!("{source}/{}", e.category);
        }
        if !e.process.is_empty() {
            source = if source.is_empty() {
                e.process.clone()
            } else {
                format!("{} {source}", e.process)
            };
        }
        if let Some(pid) = e.pid {
            source = format!("{source}[{pid}]");
        }
        if !source.is_empty() {
            out.push(' ');
            out.push_str(&source);
        }
        out.push_str(": ");
        out.push_str(&e.message);
        out.push('\n');
    }
    out
}

/// Implements the MCP tools by forwarding them to the GPUI side.
pub struct McpHost {
    sessions: Sessions,
    requests: mpsc::UnboundedSender<McpRequest>,
    timeouts: Timeouts,
    // Only drives the reply timeouts: worker threads block on it, never on the UI thread.
    timer: tokio::runtime::Runtime,
}

impl McpHost {
    pub fn new(
        sessions: Sessions,
        requests: mpsc::UnboundedSender<McpRequest>,
        timeouts: Timeouts,
    ) -> anyhow::Result<Self> {
        let timer = tokio::runtime::Builder::new_current_thread()
            .enable_time()
            .build()
            .context("cannot start the MCP timer runtime")?;
        Ok(Self {
            sessions,
            requests,
            timeouts,
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

    fn input(&self, ws: WorkspaceId, input: Input) -> Result<(), ToolError> {
        self.ask("input", self.timeouts.call, |reply| McpRequest::Input {
            ws,
            input,
            reply,
        })
    }

    fn swipe(
        &self,
        ws: WorkspaceId,
        from: (f64, f64),
        to: (f64, f64),
        duration_ms: u64,
    ) -> Result<(), ToolError> {
        let mut last = from;
        for (input, delay) in swipe_inputs(from, to, duration_ms) {
            if !delay.is_zero() {
                std::thread::sleep(delay);
            }
            if let Err(err) = self.input(ws, input) {
                if !matches!(input, Input::TouchDown { .. } | Input::TouchUp { .. }) {
                    // Don't leave a finger on the screen.
                    let _ = self.input(
                        ws,
                        Input::TouchUp {
                            x: last.0,
                            y: last.1,
                        },
                    );
                }
                return Err(err);
            }
            if let Input::TouchMove { x, y } = input {
                last = (x, y);
            }
        }
        Ok(())
    }
}

impl ToolHost for McpHost {
    fn knows_session(&self, session: &str) -> bool {
        self.sessions.resolve(session).is_some()
    }

    fn agent_signal(&self, session: &str, signal: AgentSignal) -> Result<(), ToolError> {
        let ws = self
            .sessions
            .resolve(session)
            .ok_or(ToolError::UnknownSession)?;
        self.requests
            .send(McpRequest::Signal { ws, signal })
            .map_err(|_| ToolError::Failed("Hangar is shutting down".into()))
    }

    fn call(&self, session: &str, tool: Tool) -> Result<ToolOutput, ToolError> {
        let ws = self
            .sessions
            .resolve(session)
            .ok_or(ToolError::UnknownSession)?;
        let call = self.timeouts.call;
        match tool {
            Tool::DeviceInfo => {
                let status = self.ask("device_info", call, |reply| McpRequest::Status {
                    ws,
                    reply,
                })?;
                Ok(ToolOutput::device_status(&status))
            }
            Tool::Screenshot => {
                let screen =
                    self.ask("screenshot", call, |reply| McpRequest::Frame { ws, reply })?;
                let shot = encode_screenshot(&screen.frame, MAX_SIDE)
                    .map_err(|e| ToolError::Failed(format!("screenshot: {e}")))?;
                Ok(ToolOutput::screenshot(
                    shot,
                    screen.width_pt,
                    screen.height_pt,
                ))
            }
            Tool::Tap { x, y } => {
                self.input(ws, Input::Tap { x, y })?;
                Ok(ToolOutput::text(format!("Tapped at ({x}, {y}).")))
            }
            Tool::Swipe {
                from_x,
                from_y,
                to_x,
                to_y,
                duration_ms,
            } => {
                self.swipe(ws, (from_x, from_y), (to_x, to_y), duration_ms)?;
                Ok(ToolOutput::text(format!(
                    "Swiped from ({from_x}, {from_y}) to ({to_x}, {to_y}) in {duration_ms} ms."
                )))
            }
            Tool::TypeText { text } => {
                let count = text.chars().count();
                self.ask("type_text", call, |reply| McpRequest::Text {
                    ws,
                    text,
                    reply,
                })?;
                Ok(ToolOutput::text(format!("Typed {count} characters.")))
            }
            Tool::Press(press) => {
                self.input(ws, press.input())?;
                Ok(ToolOutput::text("Pressed."))
            }
            Tool::Logs {
                lines,
                min_level,
                process,
            } => {
                let entries = self.ask("logs", call, |reply| McpRequest::Logs {
                    ws,
                    lines,
                    min_level,
                    process,
                    reply,
                })?;
                Ok(ToolOutput::text(format_logs(&entries)))
            }
            Tool::BuildAndRun => {
                let summary = self.ask("build_and_run", self.timeouts.build_and_run, |reply| {
                    McpRequest::BuildAndRun { ws, reply }
                })?;
                Ok(ToolOutput::text(summary))
            }
            Tool::LaunchApp { app_id } => {
                let label = app_id.clone();
                self.ask("launch_app", call, |reply| McpRequest::Launch {
                    ws,
                    app_id,
                    reply,
                })?;
                Ok(ToolOutput::text(format!("Launched {label}.")))
            }
        }
    }
}

fn tool_error(err: &anyhow::Error) -> ToolError {
    for cause in err.chain() {
        if let Some(tool) = cause.downcast_ref::<ToolError>() {
            return tool.clone();
        }
        if let Some(hangar_devices::Error::Unsupported(what)) =
            cause.downcast_ref::<hangar_devices::Error>()
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
    }

    /// Hand it to agents as `HANGAR_MCP_SOCKET`.
    pub fn socket_path(&self) -> &Path {
        &self.socket
    }
}

impl Drop for McpHandle {
    fn drop(&mut self) {
        self.sessions.clear();
        if let Err(e) = std::fs::remove_file(&self.socket)
            && e.kind() != std::io::ErrorKind::NotFound
        {
            tracing::warn!("mcp: cannot remove {}: {e}", self.socket.display());
        }
    }
}

/// Binds `socket` (its directory must already be private, see `paths::socket_path`) and serves
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
    let host: Arc<dyn ToolHost> = Arc::new(McpHost::new(sessions.clone(), tx, timeouts)?);
    // The handle owns the socket file from here on, so a failed spawn still removes it.
    let handle = McpHandle {
        socket: socket.to_owned(),
        sessions,
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
