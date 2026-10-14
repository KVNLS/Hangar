//! Agent presets as the app sees them: which are installed and which one is the default.

use std::ffi::OsStr;
use std::path::{Path, PathBuf};

use anyhow::Context as _;
use hangar_agents::acp::{AcpAdapter, McpServer, acp_presets};
use hangar_agents::{PtyAdapter, find_program, presets};
use hangar_core::{AgentKind, WorkspaceId};
use serde_json::{Map, Value, json};

pub const DEFAULT_AGENT: &str = "claude";
pub const FALLBACK_AGENT: &str = "shell";

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AgentChoice {
    pub id: String,
    pub name: String,
    pub installed: bool,
    /// The program as found on `PATH`.
    pub path: Option<PathBuf>,
}

/// A built-in agent: an interactive CLI in a terminal, or a structured agent over ACP.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AgentPreset {
    Pty(PtyAdapter),
    Acp(AcpAdapter),
}

impl AgentPreset {
    pub fn id(&self) -> &str {
        match self {
            Self::Pty(a) => &a.id,
            Self::Acp(a) => &a.id,
        }
    }

    pub fn display_name(&self) -> &str {
        match self {
            Self::Pty(a) => &a.display_name,
            Self::Acp(a) => &a.display_name,
        }
    }

    pub fn program(&self) -> &str {
        match self {
            Self::Pty(a) => &a.program,
            Self::Acp(a) => &a.program,
        }
    }
}

/// Every preset in picker order: each ACP variant follows the CLI it drives.
pub fn all_presets() -> Vec<AgentPreset> {
    let mut acp = acp_presets();
    let mut all = Vec::new();
    for pty in presets() {
        let id = pty.id.clone();
        all.push(AgentPreset::Pty(pty));
        let (follow, rest): (Vec<_>, Vec<_>) = acp
            .into_iter()
            .partition(|a| a.id.strip_suffix("-acp") == Some(id.as_str()));
        all.extend(follow.into_iter().map(AgentPreset::Acp));
        acp = rest;
    }
    all.extend(acp.into_iter().map(AgentPreset::Acp));
    all
}

/// Presets with their install state, resolved against `path` (a `PATH`-style list).
pub fn choices(path: Option<&str>) -> Vec<AgentChoice> {
    all_presets()
        .into_iter()
        .map(|preset| {
            let found = find_program(preset.program(), path.map(OsStr::new));
            AgentChoice {
                installed: found.is_some(),
                path: found,
                id: preset.id().to_owned(),
                name: preset.display_name().to_owned(),
            }
        })
        .collect()
}

pub fn is_installed(program: &str, path: Option<&str>) -> bool {
    find_program(program, path.map(OsStr::new)).is_some()
}

/// Claude Code if installed, else the shell.
pub fn default_agent(choices: &[AgentChoice]) -> &'static str {
    let claude = choices.iter().any(|c| c.id == DEFAULT_AGENT && c.installed);
    if claude {
        DEFAULT_AGENT
    } else {
        FALLBACK_AGENT
    }
}

pub fn preset(id: &str) -> Option<AgentPreset> {
    all_presets().into_iter().find(|preset| preset.id() == id)
}

/// How an agent is given the `hangar` MCP server (see [`mcp_launch`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum McpWiring {
    /// Only the bridge environment: no known per-process MCP mechanism.
    EnvOnly,
    /// Claude Code: `--mcp-config <inline JSON>`.
    ClaudeFlag,
    /// Structured agents: the server goes into ACP `session/new`.
    AcpSession,
    /// OpenCode: inline config in `OPENCODE_CONFIG_CONTENT`.
    OpenCodeEnv,
    /// Codex CLI: `-c mcp_servers.hangar=…`.
    CodexOverride,
    /// Gemini CLI: a system defaults file and `--skip-trust`.
    GeminiDefaults,
}

/// What the app knows about a built-in agent beyond how to start it. Adding an agent means
/// adding its preset and a row in [`PROFILES`]; nothing else matches on agent ids.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AgentProfile {
    pub id: &'static str,
    pub kind: AgentKind,
    pub mcp: McpWiring,
    /// Can run on Ollama models through [`apply_ollama`].
    pub ollama: bool,
    /// Tells Hangar when it waits for the user (hooks or ACP), so other agents may prompt it
    /// without answering its prompts (see `usecases::orchestration::check_prompt`).
    pub reports_waits: bool,
}

const fn profile(
    id: &'static str,
    kind: AgentKind,
    mcp: McpWiring,
    ollama: bool,
    reports_waits: bool,
) -> AgentProfile {
    AgentProfile {
        id,
        kind,
        mcp,
        ollama,
        reports_waits,
    }
}

pub const PROFILES: &[AgentProfile] = &[
    // Terminal Claude Code reports waits through the hooks Hangar injects.
    profile(
        "claude",
        AgentKind::Claude,
        McpWiring::ClaudeFlag,
        false,
        true,
    ),
    profile(
        "claude-acp",
        AgentKind::Claude,
        McpWiring::AcpSession,
        false,
        true,
    ),
    profile(
        "codex",
        AgentKind::Codex,
        McpWiring::CodexOverride,
        false,
        false,
    ),
    profile(
        "codex-acp",
        AgentKind::Codex,
        McpWiring::AcpSession,
        false,
        true,
    ),
    profile(
        "opencode",
        AgentKind::OpenCode,
        McpWiring::OpenCodeEnv,
        true,
        false,
    ),
    profile(
        "opencode-acp",
        AgentKind::OpenCode,
        McpWiring::AcpSession,
        true,
        true,
    ),
    profile(
        "gemini",
        AgentKind::Gemini,
        McpWiring::GeminiDefaults,
        false,
        false,
    ),
    profile(
        "gemini-acp",
        AgentKind::Gemini,
        McpWiring::AcpSession,
        false,
        true,
    ),
    profile("shell", AgentKind::Shell, McpWiring::EnvOnly, false, false),
];

/// The profile of a built-in agent; `None` for custom commands.
pub fn agent_profile(id: &str) -> Option<&'static AgentProfile> {
    PROFILES.iter().find(|p| p.id == id)
}

pub fn kind(id: &str) -> AgentKind {
    agent_profile(id).map_or_else(|| AgentKind::Custom(id.to_owned()), |p| p.kind.clone())
}

/// What a session adds on top of the agent preset: arguments, environment, and files to write
/// (mode 0600, outside the worktree) before spawning.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct AgentExtras {
    pub args: Vec<String>,
    pub env: Vec<(String, String)>,
    pub files: Vec<(PathBuf, String)>,
    /// Given to an ACP agent's `session/new`.
    pub mcp_servers: Vec<McpServer>,
    /// The first input the agent is given (the workspace task), recorded with the session.
    pub prompt: Option<String>,
}

/// How an agent reaches the app's MCP server through `Hangar mcp-bridge`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BridgeLaunch {
    /// The Hangar executable (`std::env::current_exe()`).
    pub exe: PathBuf,
    pub socket: PathBuf,
    /// The workspace's session secret; only ever passed through the environment.
    pub session: String,
    /// Hangar's per-workspace MCP config directory (`paths::mcp_config_dir`).
    pub config_dir: PathBuf,
    pub workspace: WorkspaceId,
}

const OPENCODE_CONFIG_ENV: &str = "OPENCODE_CONFIG_CONTENT";

/// Whether `agent_id` can run on Ollama models through [`apply_ollama`].
pub fn supports_ollama(agent_id: &str) -> bool {
    agent_profile(agent_id).is_some_and(|p| p.ollama)
}

/// Adds `provider` (from `hangar_agents::ollama::opencode_provider`) to an OpenCode agent's
/// inline config, merged into the MCP config `mcp_launch` may already have put there. Other
/// agents are left alone.
pub fn apply_ollama(
    extras: &mut AgentExtras,
    agent_id: &str,
    provider: &Map<String, Value>,
) -> anyhow::Result<()> {
    if !supports_ollama(agent_id) {
        return Ok(());
    }
    let existing = extras
        .env
        .iter()
        .position(|(name, _)| name == OPENCODE_CONFIG_ENV);
    let mut config = match existing {
        Some(i) => match serde_json::from_str(&extras.env.remove(i).1)
            .context("cannot read the OpenCode config")?
        {
            Value::Object(config) => config,
            _ => anyhow::bail!("the OpenCode config is not a JSON object"),
        },
        None => Map::from_iter([(
            "$schema".to_owned(),
            Value::from("https://opencode.ai/config.json"),
        )]),
    };
    config.extend(provider.clone());
    extras.env.push((
        OPENCODE_CONFIG_ENV.to_owned(),
        Value::Object(config).to_string(),
    ));
    Ok(())
}

const BRIDGE_ARG: &str = "mcp-bridge";
const SERVER_NAME: &str = "hangar";

/// Arguments, environment and files that give `agent_id` the `hangar` MCP server, plus
/// `device_env` (`HANGAR_DEVICE_PLATFORM`, `HANGAR_DEVICE_ID`, `ANDROID_SERIAL`). Every agent,
/// the shell included, gets `HANGAR_MCP_SESSION` and `HANGAR_MCP_SOCKET`; agents without a known
/// per-process MCP mechanism get no config. Nothing goes inside the worktree.
pub fn mcp_launch(
    agent_id: &str,
    bridge: &BridgeLaunch,
    device_env: &[(String, String)],
) -> anyhow::Result<AgentExtras> {
    let exe = utf8(&bridge.exe, "Hangar executable")?;
    let socket = utf8(&bridge.socket, "MCP socket")?;
    let mut extras = AgentExtras {
        env: device_env.to_vec(),
        ..AgentExtras::default()
    };
    extras
        .env
        .push((hangar_mcp::SESSION_ENV.to_owned(), bridge.session.clone()));
    extras
        .env
        .push((hangar_mcp::SOCKET_ENV.to_owned(), socket.to_owned()));
    let stdio_server =
        json!({ "mcpServers": { SERVER_NAME: { "command": exe, "args": [BRIDGE_ARG] } } });
    let wiring = agent_profile(agent_id).map_or(McpWiring::EnvOnly, |p| p.mcp);
    match wiring {
        McpWiring::ClaudeFlag => {
            extras.args.push("--mcp-config".to_owned());
            extras.args.push(stdio_server.to_string());
            // Hooks report waits and finished turns, which output timing cannot tell.
            extras.args.push("--settings".to_owned());
            extras.args.push(claude_hooks(exe).to_string());
        }
        McpWiring::AcpSession => extras.mcp_servers.push(McpServer {
            name: SERVER_NAME.to_owned(),
            command: exe.to_owned(),
            args: vec![BRIDGE_ARG.to_owned()],
            env: vec![
                (hangar_mcp::SESSION_ENV.to_owned(), bridge.session.clone()),
                (hangar_mcp::SOCKET_ENV.to_owned(), socket.to_owned()),
            ],
        }),
        McpWiring::OpenCodeEnv => {
            let config = json!({
                "$schema": "https://opencode.ai/config.json",
                "mcp": { SERVER_NAME: { "type": "local", "command": [exe, BRIDGE_ARG], "enabled": true } },
            });
            extras
                .env
                .push((OPENCODE_CONFIG_ENV.to_owned(), config.to_string()));
        }
        McpWiring::CodexOverride => {
            extras.args.push("-c".to_owned());
            // Codex gives MCP tools 60 s by default; `ask_user` waits for the user (30 min).
            extras.args.push(format!(
                "mcp_servers.{SERVER_NAME}={{command={},args=[{}],env_vars=[{},{}],tool_timeout_sec=1800}}",
                toml_string(exe),
                toml_string(BRIDGE_ARG),
                toml_string(hangar_mcp::SESSION_ENV),
                toml_string(hangar_mcp::SOCKET_ENV),
            ));
        }
        McpWiring::GeminiDefaults => {
            let file = bridge
                .config_dir
                .join(format!("{}.gemini.json", bridge.workspace));
            let path = utf8(&file, "Gemini settings file")?.to_owned();
            let contents = serde_json::to_string_pretty(&stdio_server)
                .context("cannot encode the Gemini MCP settings")?;
            extras.files.push((file, contents));
            extras
                .env
                .push(("GEMINI_CLI_SYSTEM_DEFAULTS_PATH".to_owned(), path));
            extras.args.push("--skip-trust".to_owned());
        }
        McpWiring::EnvOnly => {}
    }
    Ok(extras)
}

/// Claude Code settings that run `Hangar notify <event>` on the events that change the agent's
/// state (see `hangar_mcp::hook`). Added to the user's own settings, not replacing them.
fn claude_hooks(exe: &str) -> Value {
    let hooks: Map<String, Value> = hangar_mcp::hook::HOOK_EVENTS
        .iter()
        .map(|event| {
            let command = json!({
                "type": "command",
                "command": format!("{} notify {event}", shell_quote(exe)),
                "timeout": 5,
            });
            let entry = match *event {
                "PreToolUse" | "PostToolUse" | "PermissionRequest" => {
                    json!({ "matcher": "*", "hooks": [command] })
                }
                _ => json!({ "hooks": [command] }),
            };
            ((*event).to_owned(), json!([entry]))
        })
        .collect();
    json!({ "hooks": hooks })
}

/// `s` as one POSIX shell word.
fn shell_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', r"'\''"))
}

fn utf8<'a>(path: &'a Path, what: &str) -> anyhow::Result<&'a str> {
    path.to_str()
        .with_context(|| format!("the {what} path is not UTF-8: {}", path.display()))
}

/// A TOML basic string literal.
fn toml_string(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\t' => out.push_str("\\t"),
            '\r' => out.push_str("\\r"),
            c if c.is_control() => out.push_str(&format!("\\u{:04X}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

#[cfg(test)]
mod tests;
