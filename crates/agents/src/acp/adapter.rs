//! Starts an ACP agent as a child process and drives one session on it.

use std::collections::VecDeque;
use std::io::Read;
use std::os::unix::process::CommandExt as _;
use std::path::{Path, PathBuf};
use std::process::{ChildStdin, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::Receiver;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use super::client::{AcpConnection, AcpEvent, RequestId};
use super::protocol::{
    self, AUTH_REQUIRED, AgentInfo, ConfigOption, McpServer, PROTOCOL_VERSION, SessionMode,
};
use crate::SpawnContext;
use crate::error::{Error, Result};
use crate::path::{find_program, resolve_program};

const INITIALIZE_TIMEOUT: Duration = Duration::from_secs(60);
/// `session/new` starts the session's MCP servers, which can be slow.
const NEW_SESSION_TIMEOUT: Duration = Duration::from_secs(120);
const SET_MODE_TIMEOUT: Duration = Duration::from_secs(10);
/// Switching models can make the agent fetch model metadata.
const SET_CONFIG_TIMEOUT: Duration = Duration::from_secs(30);
const STDERR_TAIL: usize = 4096;
/// How long to wait for the rest of an exited agent's stderr before reporting it.
const STDERR_DRAIN: Duration = Duration::from_secs(1);
/// How long a stopped agent gets between SIGTERM and SIGKILL.
const KILL_GRACE: Duration = Duration::from_secs(3);

/// Runs an agent that speaks the Agent Client Protocol on its stdio.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AcpAdapter {
    pub id: String,
    pub display_name: String,
    /// Bare name looked up on `PATH`, or a path containing `/`.
    pub program: String,
    pub args: Vec<String>,
    pub env: Vec<(String, String)>,
    /// Shown when the program is missing.
    pub install_hint: String,
    /// Shown when the agent refuses to start a session until the user signs in.
    pub login_hint: String,
}

impl AcpAdapter {
    pub fn new(
        id: impl Into<String>,
        display_name: impl Into<String>,
        program: impl Into<String>,
    ) -> Self {
        Self {
            id: id.into(),
            display_name: display_name.into(),
            program: program.into(),
            args: Vec::new(),
            env: Vec::new(),
            install_hint: String::new(),
            login_hint: String::new(),
        }
    }

    pub fn with_args<S: Into<String>>(mut self, args: impl IntoIterator<Item = S>) -> Self {
        self.args = args.into_iter().map(Into::into).collect();
        self
    }

    pub fn with_install_hint(mut self, hint: impl Into<String>) -> Self {
        self.install_hint = hint.into();
        self
    }

    pub fn with_login_hint(mut self, hint: impl Into<String>) -> Self {
        self.login_hint = hint.into();
        self
    }

    pub fn is_installed(&self) -> bool {
        find_program(&self.program, std::env::var_os("PATH").as_deref()).is_some()
    }

    /// Spawns the agent, then runs `initialize` and opens a session for `ctx.cwd`: `resume` (a
    /// session id from an earlier [`AcpSession::session_id`]) is loaded with `session/load` when
    /// the agent supports it, its history arriving as updates; otherwise, or when loading fails,
    /// a new session is created and [`AcpSession::resume_error`] says why. Blocks for the
    /// handshake (up to a few minutes), so call it off the UI thread. `ctx.on_update` runs
    /// after every event sent to the returned receiver.
    pub fn start(
        &self,
        ctx: &SpawnContext,
        mcp_servers: &[McpServer],
        resume: Option<&str>,
    ) -> Result<(AcpSession, Receiver<AcpEvent>)> {
        if !ctx.cwd.is_dir() {
            return Err(Error::InvalidCwd(ctx.cwd.clone()));
        }
        let program = resolve_program(&self.id, &self.program, &self.env, &ctx.env)?;
        let mut command = Command::new(&program);
        command
            .args(self.args.iter().chain(&ctx.extra_args))
            .current_dir(&ctx.cwd)
            .envs(self.env.iter().chain(&ctx.env).map(|(k, v)| (k, v)))
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            // Own process group, so stopping the agent also stops what it started.
            .process_group(0);
        let mut child = command.spawn()?;
        let pid = child.id();
        spawn_watchdog(std::process::id(), pid);
        let (Some(stdin), Some(stdout), Some(stderr)) =
            (child.stdin.take(), child.stdout.take(), child.stderr.take())
        else {
            return Err(Error::Protocol("agent stdio was not piped".to_owned()));
        };

        let tail = Arc::new(Mutex::new(VecDeque::with_capacity(STDERR_TAIL)));
        let tail_writer = Arc::clone(&tail);
        let agent_id = self.id.clone();
        let stderr_closed = Arc::new(AtomicBool::new(false));
        let closed_writer = Arc::clone(&stderr_closed);
        std::thread::Builder::new()
            .name("acp-stderr".into())
            .spawn(move || {
                collect_stderr(stderr, &tail_writer, &agent_id);
                closed_writer.store(true, Ordering::SeqCst);
            })?;

        let exited = Arc::new(AtomicBool::new(false));
        let on_close = {
            let tail = Arc::clone(&tail);
            let exited = Arc::clone(&exited);
            Box::new(move || {
                let code = match child.wait() {
                    Ok(status) => status.code(),
                    Err(e) => {
                        tracing::debug!("acp: wait failed: {e}");
                        None
                    }
                };
                exited.store(true, Ordering::SeqCst);
                AcpEvent::Exited {
                    code,
                    stderr_tail: read_tail(&tail),
                }
            })
        };
        let (conn, events) = AcpConnection::start(stdout, stdin, ctx.on_update.clone(), on_close)?;

        let mut session = AcpSession {
            agent_id: self.id.clone(),
            program,
            pid,
            session_id: String::new(),
            info: AgentInfo::default(),
            current_mode: None,
            modes: Vec::new(),
            config: Vec::new(),
            resumed: false,
            resume_error: None,
            conn,
            exited,
            tail,
            stderr_closed,
        };
        match session.handshake(&ctx.cwd, mcp_servers, resume) {
            Ok(()) => Ok((session, events)),
            Err(e) => {
                session.kill();
                Err(self.explain_auth(session.explain(e)))
            }
        }
    }
}

impl AcpAdapter {
    fn explain_auth(&self, e: Error) -> Error {
        match e {
            Error::Rpc {
                code: AUTH_REQUIRED,
                message,
                ..
            } => Error::NotSignedIn {
                agent: self.display_name.clone(),
                message: message.trim_end_matches('.').to_owned(),
                hint: self.login_hint.clone(),
            },
            other => other,
        }
    }
}

/// A running ACP agent with one open session.
pub struct AcpSession {
    agent_id: String,
    program: PathBuf,
    pid: u32,
    session_id: String,
    info: AgentInfo,
    current_mode: Option<String>,
    modes: Vec<SessionMode>,
    config: Vec<ConfigOption>,
    resumed: bool,
    resume_error: Option<String>,
    conn: AcpConnection<ChildStdin>,
    exited: Arc<AtomicBool>,
    tail: Arc<Mutex<VecDeque<u8>>>,
    /// The agent's stderr reached EOF: [`AcpSession::tail`] is complete.
    stderr_closed: Arc<AtomicBool>,
}

impl AcpSession {
    fn handshake(
        &mut self,
        cwd: &Path,
        mcp_servers: &[McpServer],
        resume: Option<&str>,
    ) -> Result<()> {
        let result = self.conn.request(
            "initialize",
            protocol::initialize_params(),
            INITIALIZE_TIMEOUT,
        )?;
        self.info = protocol::parse_initialize(&result);
        if self.info.protocol_version != PROTOCOL_VERSION {
            return Err(Error::Protocol(format!(
                "agent speaks ACP version {}, Hangar speaks {PROTOCOL_VERSION}",
                self.info.protocol_version
            )));
        }
        let cwd = cwd.to_str().ok_or_else(|| {
            Error::Protocol(format!("working directory is not UTF-8: {}", cwd.display()))
        })?;
        if let Some(id) = resume {
            if !self.info.load_session {
                self.resume_error = Some("the agent cannot resume sessions".to_owned());
            } else {
                match self.conn.request(
                    "session/load",
                    protocol::load_session_params(id, cwd, mcp_servers),
                    NEW_SESSION_TIMEOUT,
                ) {
                    Ok(result) => {
                        self.session_id = id.to_owned();
                        self.resumed = true;
                        (self.current_mode, self.modes) = protocol::parse_modes(&result);
                        self.config = protocol::parse_config_options(&result["configOptions"]);
                        return Ok(());
                    }
                    Err(e @ Error::Rpc { .. }) => self.resume_error = Some(e.to_string()),
                    Err(e) => return Err(e),
                }
            }
        }
        let result = self.conn.request(
            "session/new",
            protocol::new_session_params(cwd, mcp_servers),
            NEW_SESSION_TIMEOUT,
        )?;
        self.session_id = result["sessionId"]
            .as_str()
            .ok_or_else(|| Error::Protocol("session/new returned no sessionId".to_owned()))?
            .to_owned();
        (self.current_mode, self.modes) = protocol::parse_modes(&result);
        self.config = protocol::parse_config_options(&result["configOptions"]);
        Ok(())
    }

    /// Adds what the agent printed on stderr to errors caused by it going away.
    /// Turns "the pipe closed" into "the agent exited", with its last stderr lines. Writing the
    /// first request to an agent that already exited fails with `BrokenPipe` instead of
    /// `Disconnected`, depending on which side notices first.
    fn explain(&self, e: Error) -> Error {
        match e {
            Error::Disconnected { .. } => self.exited_error(),
            Error::Io(io) if io.kind() == std::io::ErrorKind::BrokenPipe => self.exited_error(),
            other => other,
        }
    }

    fn exited_error(&self) -> Error {
        let deadline = std::time::Instant::now() + STDERR_DRAIN;
        while !self.stderr_closed.load(Ordering::SeqCst) && std::time::Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(10));
        }
        Error::AgentExited {
            agent: self.agent_id.clone(),
            stderr_tail: read_tail(&self.tail),
        }
    }

    pub fn agent_id(&self) -> &str {
        &self.agent_id
    }

    /// Resolved executable that was started.
    pub fn program(&self) -> &Path {
        &self.program
    }

    pub fn pid(&self) -> u32 {
        self.pid
    }

    pub fn session_id(&self) -> &str {
        &self.session_id
    }

    pub fn info(&self) -> &AgentInfo {
        &self.info
    }

    /// The mode the session started in; later changes arrive as `CurrentMode` updates.
    pub fn initial_mode(&self) -> Option<&str> {
        self.current_mode.as_deref()
    }

    pub fn modes(&self) -> &[SessionMode] {
        &self.modes
    }

    /// Starts a prompt turn. The agent streams [`AcpEvent::Update`]s and ends the turn with
    /// [`AcpEvent::TurnEnded`] or [`AcpEvent::TurnFailed`].
    pub fn prompt(&self, text: &str) -> Result<()> {
        self.conn
            .send_prompt(protocol::prompt_params(&self.session_id, text))
    }

    /// Switches the session to one of [`Self::modes`]. Blocks until the agent answers.
    pub fn set_mode(&self, mode_id: &str) -> Result<()> {
        if !self.modes.iter().any(|m| m.id == mode_id) {
            return Err(Error::Protocol(format!(
                "the agent offers no mode `{mode_id}`"
            )));
        }
        self.conn.request(
            "session/set_mode",
            protocol::set_mode_params(&self.session_id, mode_id),
            SET_MODE_TIMEOUT,
        )?;
        Ok(())
    }

    /// [`Self::set_mode`] before the session is shared; [`Self::initial_mode`] and the mode
    /// config option follow, since agents do not always send an update for it.
    pub fn apply_mode(&mut self, mode_id: &str) -> Result<()> {
        if self.current_mode.as_deref() == Some(mode_id) {
            return Ok(());
        }
        self.set_mode(mode_id)?;
        self.current_mode = Some(mode_id.to_owned());
        for option in self.config.iter_mut().filter(|o| o.is_mode()) {
            option.current = mode_id.to_owned();
        }
        Ok(())
    }

    /// The config options the session started with; later lists arrive as `ConfigOptions`
    /// updates or from [`Self::set_config_option`].
    pub fn config_options(&self) -> &[ConfigOption] {
        &self.config
    }

    /// Sets a config option. Blocks until the agent answers with the complete new list, which
    /// can add or drop options (picking a model changes the efforts on offer).
    pub fn set_config_option(&self, config_id: &str, value: &str) -> Result<Vec<ConfigOption>> {
        let result = self.conn.request(
            "session/set_config_option",
            protocol::set_config_option_params(&self.session_id, config_id, value),
            SET_CONFIG_TIMEOUT,
        )?;
        Ok(protocol::parse_config_options(&result["configOptions"]))
    }

    /// Applies saved `(config id, value)` choices before the session is shared: model options
    /// first (they decide which efforts exist), then the rest in order. Choices the agent does
    /// not offer (any more) are skipped; returns the ones it refused.
    pub fn apply_config(&mut self, wanted: &[(String, String)]) -> Vec<String> {
        let is_model = |id: &str, config: &[ConfigOption]| {
            config
                .iter()
                .any(|o| o.id == id && o.category.as_deref() == Some("model"))
        };
        let mut ordered: Vec<&(String, String)> = wanted.iter().collect();
        ordered.sort_by_key(|(id, _)| !is_model(id, &self.config));
        let mut refused = Vec::new();
        for (id, value) in ordered {
            let offered = self.config.iter().any(|o| {
                &o.id == id && &o.current != value && o.values.iter().any(|v| &v.value == value)
            });
            if !offered {
                continue;
            }
            match self.set_config_option(id, value) {
                Ok(list) if !list.is_empty() => self.config = list,
                Ok(_) => {}
                Err(e) => refused.push(format!("{id} = {value}: {e}")),
            }
        }
        refused
    }

    /// Asks the agent to stop the running turn. Unanswered permission requests are cancelled.
    pub fn cancel(&self) -> Result<()> {
        self.conn.cancel_permissions()?;
        self.conn
            .notify("session/cancel", protocol::cancel_params(&self.session_id))
    }

    /// Ends the running turn on this side when the agent did not answer its cancel.
    pub fn abandon_turn(&self) {
        self.conn.abandon_prompts();
    }

    /// `Some(option id)` selects that option, `None` cancels the request.
    pub fn answer_permission(&self, request: &RequestId, option_id: Option<&str>) -> Result<()> {
        self.conn.answer_permission(request, option_id)
    }

    /// Whether the session was resumed (`session/load`) rather than created.
    pub fn resumed(&self) -> bool {
        self.resumed
    }

    /// Why a requested resume did not happen (a new session was created instead).
    pub fn resume_error(&self) -> Option<&str> {
        self.resume_error.as_deref()
    }

    pub fn has_exited(&self) -> bool {
        self.exited.load(Ordering::SeqCst)
    }

    /// Waits up to `timeout` for the agent process to be reaped. Returns whether it was.
    pub fn wait_for_exit(&self, timeout: Duration) -> bool {
        let deadline = std::time::Instant::now() + timeout;
        while !self.has_exited() {
            if std::time::Instant::now() >= deadline {
                return false;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        true
    }

    /// Closes stdin, sends SIGTERM to the agent's process group and SIGKILL after a grace
    /// period if it is still running. [`AcpEvent::Exited`] follows.
    pub fn kill(&self) {
        self.conn.close();
        if self.exited.load(Ordering::SeqCst) {
            return;
        }
        let Ok(pgid) = libc::pid_t::try_from(self.pid) else {
            return;
        };
        // SAFETY: plain syscall; the group is ours (spawned with `process_group(0)`).
        unsafe { libc::killpg(pgid, libc::SIGTERM) };
        let exited = Arc::clone(&self.exited);
        let spawned = std::thread::Builder::new()
            .name("acp-kill".into())
            .spawn(move || {
                std::thread::sleep(KILL_GRACE);
                if !exited.load(Ordering::SeqCst) {
                    // SAFETY: as above; the leader has not been reaped, so the id is still ours.
                    unsafe { libc::killpg(pgid, libc::SIGKILL) };
                }
            });
        if let Err(e) = spawned {
            tracing::warn!("acp: could not schedule SIGKILL for {pgid}: {e}");
        }
    }
}

impl std::fmt::Debug for AcpSession {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AcpSession")
            .field("agent_id", &self.agent_id)
            .field("program", &self.program)
            .field("pid", &self.pid)
            .field("session_id", &self.session_id)
            .finish()
    }
}

/// Starts a watchdog in the agent's process group: if `owner` (Hangar) dies without stopping
/// the agent (crash, SIGKILL), it signals the whole group, since some agents (codex-acp) do not
/// exit on a closed stdin. It leaves quietly once the agent is gone, and a normal stop
/// ([`AcpSession::kill`]) takes it down with the group.
fn spawn_watchdog(owner: u32, agent: u32) {
    let script = format!(
        "while kill -0 {owner} 2>/dev/null && kill -0 {agent} 2>/dev/null; do sleep 2; done; \
         kill -0 {agent} 2>/dev/null || exit 0; kill -TERM 0; sleep 3; kill -KILL 0"
    );
    let Ok(group) = i32::try_from(agent) else {
        return;
    };
    let spawned = Command::new("/bin/sh")
        .args(["-c", &script])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .process_group(group)
        .spawn();
    match spawned {
        Ok(mut watchdog) => {
            // Reaped when it ends, so it never lingers as a zombie.
            let reaper = std::thread::Builder::new()
                .name("acp-watchdog".into())
                .spawn(move || {
                    let _ = watchdog.wait();
                });
            if let Err(e) = reaper {
                tracing::warn!("acp: watchdog reaper not started: {e}");
            }
        }
        Err(e) => tracing::warn!("acp: no watchdog for agent {agent}: {e}"),
    }
}

fn collect_stderr(mut stderr: impl Read, tail: &Mutex<VecDeque<u8>>, agent: &str) {
    let mut buf = [0u8; 4096];
    loop {
        match stderr.read(&mut buf) {
            Ok(0) | Err(_) => break,
            Ok(n) => {
                tracing::debug!(
                    "acp {agent} stderr: {}",
                    String::from_utf8_lossy(&buf[..n]).trim_end()
                );
                let mut tail = tail.lock().unwrap_or_else(PoisonError::into_inner);
                tail.extend(&buf[..n]);
                let excess = tail.len().saturating_sub(STDERR_TAIL);
                tail.drain(..excess);
            }
        }
    }
}

fn read_tail(tail: &Mutex<VecDeque<u8>>) -> String {
    let tail = tail.lock().unwrap_or_else(PoisonError::into_inner);
    let bytes: Vec<u8> = tail.iter().copied().collect();
    String::from_utf8_lossy(&bytes).trim().to_owned()
}

/// Built-in ACP agents.
pub fn acp_presets() -> Vec<AcpAdapter> {
    vec![
        AcpAdapter::new("claude-acp", "Claude Code (structured)", "claude-agent-acp")
            .with_install_hint("npm install -g @agentclientprotocol/claude-agent-acp")
            .with_login_hint("Run `claude` in a terminal and sign in."),
        AcpAdapter::new("codex-acp", "Codex (structured)", "codex-acp")
            .with_install_hint("npm install -g @zed-industries/codex-acp")
            .with_login_hint("Run `codex login` in a terminal, or set OPENAI_API_KEY."),
        AcpAdapter::new("opencode-acp", "OpenCode (structured)", "opencode")
            .with_args(["acp"])
            .with_install_hint("curl -fsSL https://opencode.ai/install | bash")
            .with_login_hint("Run `opencode auth login` in a terminal."),
        // Nobody can answer the folder trust prompt over ACP; the PTY preset skips it too.
        AcpAdapter::new("gemini-acp", "Gemini CLI (structured)", "gemini")
            .with_args(["--acp", "--skip-trust"])
            .with_install_hint("npm install -g @google/gemini-cli")
            // Google sign-in for individuals was retired (Gemini Code Assist → Antigravity).
            .with_login_hint(
                "Set GEMINI_API_KEY, or run `gemini` in a terminal and choose an API key or Vertex AI.",
            ),
    ]
}

#[cfg(test)]
mod tests;
