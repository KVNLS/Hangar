use std::ffi::OsString;
use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use hangar_terminal::{GridSize, SpawnOptions, TerminalConfig, TerminalSession};

use crate::error::{Error, Result};
use crate::path::find_program;

/// Called from the terminal pump thread after each batch of program output.
pub type UpdateHook = Arc<dyn Fn() + Send + Sync>;

/// Where and how to start an agent.
#[derive(Clone)]
pub struct SpawnContext {
    pub cwd: PathBuf,
    pub size: GridSize,
    /// Added on top of the adapter's env; wins on conflicts.
    pub env: Vec<(String, String)>,
    /// Appended after the adapter's own arguments.
    pub extra_args: Vec<String>,
    pub on_update: Option<UpdateHook>,
}

impl SpawnContext {
    pub fn new(cwd: impl Into<PathBuf>) -> Self {
        Self {
            cwd: cwd.into(),
            size: GridSize::default(),
            env: Vec::new(),
            extra_args: Vec::new(),
            on_update: None,
        }
    }
}

impl fmt::Debug for SpawnContext {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SpawnContext")
            .field("cwd", &self.cwd)
            .field("size", &self.size)
            .field("env", &self.env)
            .field("extra_args", &self.extra_args)
            .field("on_update", &self.on_update.is_some())
            .finish()
    }
}

/// A running agent. The app attaches a terminal view to [`AgentHandle::session`].
pub struct AgentHandle {
    agent_id: String,
    program: PathBuf,
    session: Arc<TerminalSession>,
}

impl AgentHandle {
    pub fn agent_id(&self) -> &str {
        &self.agent_id
    }

    /// Resolved executable that was started.
    pub fn program(&self) -> &Path {
        &self.program
    }

    pub fn session(&self) -> &Arc<TerminalSession> {
        &self.session
    }

    pub fn into_session(self) -> Arc<TerminalSession> {
        self.session
    }
}

impl fmt::Debug for AgentHandle {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("AgentHandle")
            .field("agent_id", &self.agent_id)
            .field("program", &self.program)
            .field("pid", &self.session.pid())
            .finish()
    }
}

pub trait AgentAdapter: Send + Sync {
    fn id(&self) -> &str;
    fn display_name(&self) -> &str;
    /// Whether the program resolves on the current process `PATH`.
    fn is_installed(&self) -> bool;
    fn spawn(&self, ctx: &SpawnContext) -> Result<AgentHandle>;
}

/// Runs an interactive CLI agent inside a pseudo-terminal.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PtyAdapter {
    pub id: String,
    pub display_name: String,
    /// Bare name looked up on `PATH`, or a path containing `/`.
    pub program: String,
    pub args: Vec<String>,
    pub env: Vec<(String, String)>,
}

impl PtyAdapter {
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
        }
    }

    pub fn with_args(mut self, args: impl IntoIterator<Item = impl Into<String>>) -> Self {
        self.args = args.into_iter().map(Into::into).collect();
        self
    }

    pub fn with_env(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        self.env.push((key.into(), value.into()));
        self
    }

    /// Resolves the program with the `PATH` the child will see: the context's, else the
    /// adapter's, else this process's.
    fn resolve(&self, ctx: &SpawnContext) -> Result<PathBuf> {
        let path_override = ctx
            .env
            .iter()
            .rev()
            .chain(self.env.iter().rev())
            .find(|(k, _)| k == "PATH")
            .map(|(_, v)| OsString::from(v));
        let path_var = path_override.or_else(|| std::env::var_os("PATH"));
        find_program(&self.program, path_var.as_deref()).ok_or_else(|| Error::NotInstalled {
            agent: self.id.clone(),
            program: self.program.clone(),
        })
    }
}

impl AgentAdapter for PtyAdapter {
    fn id(&self) -> &str {
        &self.id
    }

    fn display_name(&self) -> &str {
        &self.display_name
    }

    fn is_installed(&self) -> bool {
        find_program(&self.program, std::env::var_os("PATH").as_deref()).is_some()
    }

    fn spawn(&self, ctx: &SpawnContext) -> Result<AgentHandle> {
        if !ctx.cwd.is_dir() {
            return Err(Error::InvalidCwd(ctx.cwd.clone()));
        }
        let program = self.resolve(ctx)?;
        let options = SpawnOptions {
            program: Some(program.clone().into_os_string()),
            args: self
                .args
                .iter()
                .chain(&ctx.extra_args)
                .map(OsString::from)
                .collect(),
            cwd: Some(ctx.cwd.clone()),
            env: self
                .env
                .iter()
                .chain(&ctx.env)
                .map(|(k, v)| (OsString::from(k), OsString::from(v)))
                .collect(),
            size: ctx.size,
            cell_px: (0, 0),
        };
        let hook = ctx.on_update.clone();
        let session = TerminalSession::spawn(&options, TerminalConfig::default(), move || {
            if let Some(hook) = &hook {
                hook();
            }
        })?;
        Ok(AgentHandle {
            agent_id: self.id.clone(),
            program,
            session: Arc::new(session),
        })
    }
}

/// Built-in adapters. They start each CLI exactly as a user would, with no flags that skip
/// the agent's own permission prompts.
pub fn presets() -> Vec<PtyAdapter> {
    let shell = std::env::var("SHELL")
        .ok()
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "/bin/zsh".to_owned());
    vec![
        PtyAdapter::new("claude", "Claude Code", "claude"),
        PtyAdapter::new("codex", "Codex CLI", "codex"),
        PtyAdapter::new("opencode", "OpenCode", "opencode"),
        PtyAdapter::new("gemini", "Gemini CLI", "gemini"),
        PtyAdapter::new("shell", "Shell", shell).with_args(["-l"]),
    ]
}
