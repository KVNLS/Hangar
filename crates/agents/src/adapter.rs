use std::fmt;
use std::path::PathBuf;
use std::sync::Arc;

use crate::error::{Error, Result};
use crate::path::{find_program, resolve_program};

/// Called after each batch of agent output (ACP events; PTY output where the terminal is spawned).
pub type UpdateHook = Arc<dyn Fn() + Send + Sync>;

/// Where and how to start an agent.
#[derive(Clone)]
pub struct SpawnContext {
    pub cwd: PathBuf,
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
            .field("env", &self.env)
            .field("extra_args", &self.extra_args)
            .field("on_update", &self.on_update.is_some())
            .finish()
    }
}

/// What to run for an agent: the resolved program with its arguments, directory and extra
/// environment. Whoever owns the terminal starts it (`TerminalSession` in the app).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Launch {
    pub agent_id: String,
    /// Resolved executable.
    pub program: PathBuf,
    pub args: Vec<String>,
    pub cwd: PathBuf,
    /// Added on top of the inherited environment.
    pub env: Vec<(String, String)>,
}

pub trait AgentAdapter: Send + Sync {
    fn id(&self) -> &str;
    fn display_name(&self) -> &str;
    /// Whether the program resolves on the current process `PATH`.
    fn is_installed(&self) -> bool;
    /// Resolves the program and checks the directory; starts nothing.
    fn launch(&self, ctx: &SpawnContext) -> Result<Launch>;
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
    /// Shell command that installs the program; empty when there is none.
    pub install_hint: String,
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
            install_hint: String::new(),
        }
    }

    pub fn with_install_hint(mut self, hint: impl Into<String>) -> Self {
        self.install_hint = hint.into();
        self
    }

    pub fn with_args(mut self, args: impl IntoIterator<Item = impl Into<String>>) -> Self {
        self.args = args.into_iter().map(Into::into).collect();
        self
    }

    pub fn with_env(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        self.env.push((key.into(), value.into()));
        self
    }

    fn resolve(&self, ctx: &SpawnContext) -> Result<PathBuf> {
        resolve_program(&self.id, &self.program, &self.env, &ctx.env)
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

    fn launch(&self, ctx: &SpawnContext) -> Result<Launch> {
        if !ctx.cwd.is_dir() {
            return Err(Error::InvalidCwd(ctx.cwd.clone()));
        }
        Ok(Launch {
            agent_id: self.id.clone(),
            program: self.resolve(ctx)?,
            args: self.args.iter().chain(&ctx.extra_args).cloned().collect(),
            cwd: ctx.cwd.clone(),
            env: self.env.iter().chain(&ctx.env).cloned().collect(),
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
        PtyAdapter::new("claude", "Claude Code", "claude")
            .with_install_hint("curl -fsSL https://claude.ai/install.sh | bash"),
        PtyAdapter::new("codex", "Codex CLI", "codex")
            .with_install_hint("npm install -g @openai/codex"),
        PtyAdapter::new("opencode", "OpenCode", "opencode")
            .with_install_hint("curl -fsSL https://opencode.ai/install | bash"),
        PtyAdapter::new("gemini", "Gemini CLI", "gemini")
            .with_install_hint("npm install -g @google/gemini-cli"),
        PtyAdapter::new("shell", "Shell", shell).with_args(["-l"]),
    ]
}
