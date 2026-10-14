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
use super::protocol::{self, AUTH_REQUIRED, AgentInfo, McpServer, PROTOCOL_VERSION, SessionMode};
use crate::SpawnContext;
use crate::error::{Error, Result};
use crate::path::{find_program, resolve_program};

const INITIALIZE_TIMEOUT: Duration = Duration::from_secs(60);
/// `session/new` starts the session's MCP servers, which can be slow.
const NEW_SESSION_TIMEOUT: Duration = Duration::from_secs(120);
const SET_MODE_TIMEOUT: Duration = Duration::from_secs(10);
const STDERR_TAIL: usize = 4096;
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
    /// handshake (up to a few minutes), so call it off the UI thread. `ctx.size` is unused;
    /// `ctx.on_update` runs after every event sent to the returned receiver.
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
        let (Some(stdin), Some(stdout), Some(stderr)) =
            (child.stdin.take(), child.stdout.take(), child.stderr.take())
        else {
            return Err(Error::Protocol("agent stdio was not piped".to_owned()));
        };

        let tail = Arc::new(Mutex::new(VecDeque::with_capacity(STDERR_TAIL)));
        let tail_writer = Arc::clone(&tail);
        let agent_id = self.id.clone();
        std::thread::Builder::new()
            .name("acp-stderr".into())
            .spawn(move || collect_stderr(stderr, &tail_writer, &agent_id))?;

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
            resumed: false,
            resume_error: None,
            conn,
            exited,
            tail,
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
    resumed: bool,
    resume_error: Option<String>,
    conn: AcpConnection<ChildStdin>,
    exited: Arc<AtomicBool>,
    tail: Arc<Mutex<VecDeque<u8>>>,
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
        Ok(())
    }

    /// Adds what the agent printed on stderr to errors caused by it going away.
    fn explain(&self, e: Error) -> Error {
        match e {
            Error::Disconnected { .. } => Error::AgentExited {
                agent: self.agent_id.clone(),
                stderr_tail: read_tail(&self.tail),
            },
            other => other,
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

    /// Asks the agent to stop the running turn. Unanswered permission requests are cancelled.
    pub fn cancel(&self) -> Result<()> {
        self.conn.cancel_permissions()?;
        self.conn
            .notify("session/cancel", protocol::cancel_params(&self.session_id))
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
            .with_login_hint(
                "Run `gemini` in a terminal and choose Log in with Google, or set GEMINI_API_KEY.",
            ),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sh(script: &str) -> AcpAdapter {
        let mut a = AcpAdapter::new("t", "t", "/bin/sh");
        a.args = vec!["-c".into(), script.into()];
        a
    }

    #[test]
    fn resume_loads_or_falls_back() {
        let dir = tempfile::tempdir().expect("tempdir");
        // Loads: initialize (1), session/load (2) with one replayed update before the answer.
        let script = r#"
            read l; echo '{"jsonrpc":"2.0","id":1,"result":{"protocolVersion":1,"agentCapabilities":{"loadSession":true}}}'
            read l; case "$l" in *session/load*) ;; *) exit 9;; esac
            echo '{"jsonrpc":"2.0","method":"session/update","params":{"sessionId":"old","update":{"sessionUpdate":"user_message_chunk","content":{"type":"text","text":"hi"}}}}'
            echo '{"jsonrpc":"2.0","id":2,"result":{}}'
            read l
        "#;
        let (session, events) = sh(script)
            .start(&SpawnContext::new(dir.path()), &[], Some("old"))
            .expect("resumes");
        assert!(session.resumed());
        assert_eq!(session.session_id(), "old");
        assert!(matches!(
            events.recv_timeout(Duration::from_secs(5)),
            Ok(AcpEvent::Update(protocol::SessionUpdate::UserMessage(_)))
        ));
        session.kill();

        // Load refused: a new session is created and the reason kept.
        let script = r#"
            read l; echo '{"jsonrpc":"2.0","id":1,"result":{"protocolVersion":1,"agentCapabilities":{"loadSession":true}}}'
            read l; echo '{"jsonrpc":"2.0","id":2,"error":{"code":-32002,"message":"Session not found"}}'
            read l; echo '{"jsonrpc":"2.0","id":3,"result":{"sessionId":"new"}}'
            read l
        "#;
        let (session, _events) = sh(script)
            .start(&SpawnContext::new(dir.path()), &[], Some("gone"))
            .expect("falls back");
        assert!(!session.resumed());
        assert_eq!(session.session_id(), "new");
        assert!(
            session
                .resume_error()
                .is_some_and(|e| e.contains("Session not found"))
        );
        session.kill();
    }

    #[test]
    fn auth_required_names_the_login_step() {
        let dir = tempfile::tempdir().expect("tempdir");
        let script = r#"
            read l; echo '{"jsonrpc":"2.0","id":1,"result":{"protocolVersion":1,"authMethods":[{"id":"oauth","name":"Log in"}]}}'
            read l; echo '{"jsonrpc":"2.0","id":2,"error":{"code":-32000,"message":"API key is missing"}}'
            read l
        "#;
        let err = sh(script)
            .with_login_hint("Run `t login`.")
            .start(&SpawnContext::new(dir.path()), &[], None)
            .expect_err("needs a login");
        assert_eq!(
            err.to_string(),
            "t is not signed in (API key is missing). Run `t login`."
        );
    }

    #[test]
    fn exit_during_handshake_reports_stderr() {
        let dir = tempfile::tempdir().expect("tempdir");
        let err = sh("echo 'node: not found' >&2; exit 3")
            .start(&SpawnContext::new(dir.path()), &[], None)
            .expect_err("fails");
        match err {
            Error::AgentExited { stderr_tail, .. } => {
                assert!(stderr_tail.contains("node: not found"), "{stderr_tail}");
            }
            other => panic!("unexpected {other:?}"),
        }
    }

    #[test]
    fn scripted_agent_handshake_and_turn() {
        let dir = tempfile::tempdir().expect("tempdir");
        // Answers initialize, session/new and one prompt by request order (ids 1, 2, 3).
        let script = r#"
            read l; echo '{"jsonrpc":"2.0","id":1,"result":{"protocolVersion":1,"agentCapabilities":{"loadSession":true}}}'
            read l; echo '{"jsonrpc":"2.0","id":2,"result":{"sessionId":"s-1"}}'
            read l; echo '{"jsonrpc":"2.0","method":"session/update","params":{"sessionId":"s-1","update":{"sessionUpdate":"agent_message_chunk","content":{"type":"text","text":"done"}}}}'
            echo '{"jsonrpc":"2.0","id":3,"result":{"stopReason":"end_turn"}}'
            read l
        "#;
        let (session, events) = sh(script)
            .start(&SpawnContext::new(dir.path()), &[], None)
            .expect("starts");
        assert_eq!(session.session_id(), "s-1");
        assert!(session.info().load_session);
        let rx = &events;
        session.prompt("go").expect("prompt");
        let wait =
            |rx: &Receiver<AcpEvent>| rx.recv_timeout(Duration::from_secs(5)).expect("event");
        assert!(matches!(wait(rx), AcpEvent::Update(_)));
        assert_eq!(wait(rx), AcpEvent::TurnEnded(protocol::StopReason::EndTurn));
        session.kill();
        assert!(matches!(wait(rx), AcpEvent::Exited { .. }));
        assert!(session.wait_for_exit(Duration::from_secs(1)));
        assert!(matches!(session.set_mode("plan"), Err(Error::Protocol(_))));
    }
}
