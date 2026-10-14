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
        .map(|preset| AgentChoice {
            installed: is_installed(preset.program(), path),
            id: preset.id().to_owned(),
            name: preset.display_name().to_owned(),
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

pub fn kind(id: &str) -> AgentKind {
    match id {
        "claude" | "claude-acp" => AgentKind::Claude,
        "codex" | "codex-acp" => AgentKind::Codex,
        "opencode" | "opencode-acp" => AgentKind::OpenCode,
        "gemini" | "gemini-acp" => AgentKind::Gemini,
        "shell" => AgentKind::Shell,
        other => AgentKind::Custom(other.to_owned()),
    }
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
    matches!(agent_id, "opencode" | "opencode-acp")
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
    match agent_id {
        "claude" => {
            extras.args.push("--mcp-config".to_owned());
            extras.args.push(stdio_server.to_string());
        }
        "claude-acp" | "codex-acp" | "gemini-acp" | "opencode-acp" => {
            extras.mcp_servers.push(McpServer {
                name: SERVER_NAME.to_owned(),
                command: exe.to_owned(),
                args: vec![BRIDGE_ARG.to_owned()],
                env: vec![
                    (hangar_mcp::SESSION_ENV.to_owned(), bridge.session.clone()),
                    (hangar_mcp::SOCKET_ENV.to_owned(), socket.to_owned()),
                ],
            })
        }
        "opencode" => {
            let config = json!({
                "$schema": "https://opencode.ai/config.json",
                "mcp": { SERVER_NAME: { "type": "local", "command": [exe, BRIDGE_ARG], "enabled": true } },
            });
            extras
                .env
                .push((OPENCODE_CONFIG_ENV.to_owned(), config.to_string()));
        }
        "codex" => {
            extras.args.push("-c".to_owned());
            extras.args.push(format!(
                "mcp_servers.{SERVER_NAME}={{command={},args=[{}],env_vars=[{},{}]}}",
                toml_string(exe),
                toml_string(BRIDGE_ARG),
                toml_string(hangar_mcp::SESSION_ENV),
                toml_string(hangar_mcp::SOCKET_ENV),
            ));
        }
        "gemini" => {
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
        _ => {}
    }
    Ok(extras)
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
mod tests {
    use super::{
        AgentChoice, AgentExtras, AgentPreset, BridgeLaunch, apply_ollama, choices, default_agent,
        is_installed, kind, mcp_launch, preset, toml_string,
    };
    use hangar_core::AgentKind;
    use serde_json::{Value, json};
    use std::path::PathBuf;

    fn choice(id: &str, installed: bool) -> AgentChoice {
        AgentChoice {
            id: id.into(),
            name: id.into(),
            installed,
        }
    }

    #[test]
    fn default_prefers_installed_claude() {
        assert_eq!(
            default_agent(&[choice("claude", true), choice("shell", true)]),
            "claude"
        );
        assert_eq!(
            default_agent(&[choice("claude", false), choice("shell", true)]),
            "shell"
        );
        assert_eq!(default_agent(&[]), "shell");
    }

    #[test]
    fn installed_check_uses_given_path() {
        assert!(is_installed("sh", Some("/nonexistent:/bin")));
        assert!(!is_installed("sh", Some("/nonexistent")));
        assert!(!is_installed("sh", None));
    }

    #[test]
    fn presets_map_to_kinds() {
        let all = choices(Some("/bin:/usr/bin"));
        assert!(all.iter().any(|c| c.id == "shell"));
        for c in &all {
            assert!(preset(&c.id).is_some());
            assert!(!matches!(kind(&c.id), AgentKind::Custom(_)), "{}", c.id);
        }
        assert_eq!(kind("aider"), AgentKind::Custom("aider".into()));
        assert!(preset("nope").is_none());
        let ids: Vec<String> = all.iter().map(|c| c.id.clone()).collect();
        let at = |id: &str| ids.iter().position(|i| i == id);
        for cli in ["claude", "codex", "opencode", "gemini"] {
            let acp = format!("{cli}-acp");
            assert_eq!(at(&acp), at(cli).map(|i| i + 1), "{acp}");
            assert!(matches!(preset(&acp), Some(AgentPreset::Acp(_))), "{acp}");
        }
    }

    const EXE: &str = "/Applications/Hangar.app/Contents/MacOS/Hangar";

    fn bridge(exe: &str) -> BridgeLaunch {
        BridgeLaunch {
            exe: PathBuf::from(exe),
            socket: PathBuf::from("/tmp/hangar-501/hangar.sock"),
            session: "ab12".into(),
            config_dir: PathBuf::from("/data/mcp"),
            workspace: serde_json::from_value(json!("00000000-0000-0000-0000-000000000000"))
                .unwrap(),
        }
    }

    fn env_of<'a>(env: &'a [(String, String)], key: &str) -> Option<&'a str> {
        env.iter()
            .rev()
            .find(|(k, _)| k == key)
            .map(|(_, v)| v.as_str())
    }

    fn assert_bridge_env(env: &[(String, String)]) {
        assert_eq!(env_of(env, "HANGAR_MCP_SESSION"), Some("ab12"));
        assert_eq!(
            env_of(env, "HANGAR_MCP_SOCKET"),
            Some("/tmp/hangar-501/hangar.sock")
        );
        assert_eq!(env_of(env, "HANGAR_DEVICE_ID"), Some("UDID"));
    }

    fn device_env() -> Vec<(String, String)> {
        vec![
            ("HANGAR_DEVICE_PLATFORM".into(), "ios".into()),
            ("HANGAR_DEVICE_ID".into(), "UDID".into()),
        ]
    }

    #[test]
    fn claude_gets_inline_mcp_config() {
        let x = mcp_launch("claude", &bridge(EXE), &device_env()).unwrap();
        assert_eq!(x.args[0], "--mcp-config");
        let config: Value = serde_json::from_str(&x.args[1]).unwrap();
        assert_eq!(
            config,
            json!({ "mcpServers": { "hangar": { "command": EXE, "args": ["mcp-bridge"] } } })
        );
        assert_eq!(x.args.len(), 2);
        assert!(x.files.is_empty());
        assert!(!x.args[1].contains("ab12"), "no secret in argv");
        assert_bridge_env(&x.env);
    }

    #[test]
    fn acp_agents_get_the_bridge_through_session_new() {
        for id in ["claude-acp", "codex-acp", "gemini-acp", "opencode-acp"] {
            acp_bridge(id);
        }
    }

    fn acp_bridge(id: &str) {
        let x = mcp_launch(id, &bridge(EXE), &device_env()).unwrap();
        assert!(x.args.is_empty(), "no secret or config in argv");
        assert_eq!(x.mcp_servers.len(), 1);
        let server = &x.mcp_servers[0];
        assert_eq!(server.name, "hangar");
        assert_eq!(server.command, EXE);
        assert_eq!(server.args, ["mcp-bridge"]);
        assert_eq!(env_of(&server.env, "HANGAR_MCP_SESSION"), Some("ab12"));
        assert_eq!(
            env_of(&server.env, "HANGAR_MCP_SOCKET"),
            Some("/tmp/hangar-501/hangar.sock")
        );
        assert_bridge_env(&x.env);
    }

    fn ollama_provider() -> serde_json::Map<String, Value> {
        let models = [hangar_agents::ollama::OllamaModel {
            name: "qwen3-coder".into(),
            parameter_size: None,
        }];
        let host = hangar_agents::ollama::OllamaHost::parse("").unwrap();
        hangar_agents::ollama::opencode_provider(&host, &models, Some("qwen3-coder"))
    }

    #[test]
    fn ollama_merges_into_opencode_mcp_config() {
        let mut x = mcp_launch("opencode", &bridge(EXE), &device_env()).unwrap();
        apply_ollama(&mut x, "opencode", &ollama_provider()).unwrap();
        let configs: Vec<_> = x
            .env
            .iter()
            .filter(|(name, _)| name == "OPENCODE_CONFIG_CONTENT")
            .collect();
        assert_eq!(configs.len(), 1);
        let config: Value = serde_json::from_str(&configs[0].1).unwrap();
        assert_eq!(
            config["mcp"]["hangar"]["command"],
            json!([EXE, "mcp-bridge"])
        );
        assert_eq!(
            config["provider"]["ollama"]["options"]["baseURL"],
            "http://127.0.0.1:11434/v1"
        );
        assert_eq!(config["model"], "ollama/qwen3-coder");
        assert_bridge_env(&x.env);
    }

    #[test]
    fn ollama_gives_opencode_acp_its_own_config() {
        let mut x = mcp_launch("opencode-acp", &bridge(EXE), &device_env()).unwrap();
        apply_ollama(&mut x, "opencode-acp", &ollama_provider()).unwrap();
        let config: Value =
            serde_json::from_str(env_of(&x.env, "OPENCODE_CONFIG_CONTENT").unwrap()).unwrap();
        assert_eq!(config["$schema"], "https://opencode.ai/config.json");
        assert_eq!(config["model"], "ollama/qwen3-coder");
        assert!(config.get("mcp").is_none());
        assert_eq!(x.mcp_servers.len(), 1);
    }

    #[test]
    fn ollama_leaves_other_agents_alone() {
        for id in [
            "claude",
            "claude-acp",
            "codex",
            "codex-acp",
            "gemini",
            "shell",
        ] {
            let mut x = AgentExtras::default();
            apply_ollama(&mut x, id, &ollama_provider()).unwrap();
            assert_eq!(x, AgentExtras::default(), "{id}");
        }
    }

    #[test]
    fn opencode_gets_config_content_env() {
        let x = mcp_launch("opencode", &bridge(EXE), &device_env()).unwrap();
        assert!(x.args.is_empty() && x.files.is_empty());
        let config: Value =
            serde_json::from_str(env_of(&x.env, "OPENCODE_CONFIG_CONTENT").unwrap()).unwrap();
        assert_eq!(
            config,
            json!({
                "$schema": "https://opencode.ai/config.json",
                "mcp": { "hangar": { "type": "local", "command": [EXE, "mcp-bridge"], "enabled": true } },
            })
        );
        assert_bridge_env(&x.env);
    }

    #[test]
    fn codex_gets_escaped_toml_override() {
        let x = mcp_launch("codex", &bridge(EXE), &device_env()).unwrap();
        assert_eq!(
            x.args,
            vec![
                "-c".to_owned(),
                format!(
                    "mcp_servers.hangar={{command=\"{EXE}\",args=[\"mcp-bridge\"],env_vars=[\"HANGAR_MCP_SESSION\",\"HANGAR_MCP_SOCKET\"]}}"
                ),
            ]
        );
        assert_bridge_env(&x.env);

        let odd = r#"/Users/me/My "Apps"\bin/Hangar"#;
        let x = mcp_launch("codex", &bridge(odd), &[]).unwrap();
        assert!(
            x.args[1]
                .starts_with(r#"mcp_servers.hangar={command="/Users/me/My \"Apps\"\\bin/Hangar","#),
            "{}",
            x.args[1]
        );
        assert_eq!(toml_string("a\nb\u{1}"), r#""a\nb\u0001""#);
    }

    #[test]
    fn gemini_gets_a_settings_file_outside_the_worktree() {
        let x = mcp_launch("gemini", &bridge(EXE), &device_env()).unwrap();
        assert_eq!(x.args, vec!["--skip-trust".to_owned()]);
        let file = PathBuf::from("/data/mcp/00000000-0000-0000-0000-000000000000.gemini.json");
        assert_eq!(x.files.len(), 1);
        assert_eq!(x.files[0].0, file);
        let settings: Value = serde_json::from_str(&x.files[0].1).unwrap();
        assert_eq!(
            settings,
            json!({ "mcpServers": { "hangar": { "command": EXE, "args": ["mcp-bridge"] } } })
        );
        assert!(!x.files[0].1.contains("ab12"), "no secret in the file");
        assert_eq!(
            env_of(&x.env, "GEMINI_CLI_SYSTEM_DEFAULTS_PATH"),
            file.to_str()
        );
        assert_bridge_env(&x.env);
    }

    #[test]
    fn shell_and_custom_agents_get_env_only() {
        for id in ["shell", "aider"] {
            let x = mcp_launch(id, &bridge(EXE), &device_env()).unwrap();
            assert!(x.args.is_empty() && x.files.is_empty(), "{id}");
            assert_eq!(x.env.len(), 4);
            assert_bridge_env(&x.env);
        }
    }
}
