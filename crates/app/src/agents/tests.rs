use super::{
    AgentChoice, AgentExtras, AgentPreset, BridgeLaunch, McpWiring, PROFILES, apply_ollama,
    choices, default_agent, is_installed, kind, mcp_launch, preset, shell_quote, toml_string,
};
use hangar_core::AgentKind;
use serde_json::{Value, json};
use std::path::PathBuf;

fn choice(id: &str, installed: bool) -> AgentChoice {
    AgentChoice {
        id: id.into(),
        name: id.into(),
        installed,
        path: None,
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
    let mut profiled: Vec<&str> = PROFILES.iter().map(|p| p.id).collect();
    let mut listed: Vec<&str> = all.iter().map(|c| c.id.as_str()).collect();
    profiled.sort_unstable();
    listed.sort_unstable();
    assert_eq!(profiled, listed, "every preset has exactly one profile");
    for p in PROFILES {
        let acp = matches!(preset(p.id), Some(AgentPreset::Acp(_)));
        assert_eq!(acp, p.mcp == McpWiring::AcpSession, "{}", p.id);
    }
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
        workspace: serde_json::from_value(json!("00000000-0000-0000-0000-000000000000")).unwrap(),
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
fn shell_quote_survives_quotes_and_spaces() {
    assert_eq!(
        shell_quote("/Applications/Hangar.app/x"),
        "'/Applications/Hangar.app/x'"
    );
    assert_eq!(shell_quote("/tmp/it's here"), r"'/tmp/it'\''s here'");
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
    assert_eq!(x.args[2], "--settings");
    let settings: Value = serde_json::from_str(&x.args[3]).unwrap();
    let stop = &settings["hooks"]["Stop"][0]["hooks"][0];
    assert_eq!(stop["type"], "command");
    assert_eq!(stop["command"], format!("'{EXE}' notify Stop"));
    assert_eq!(settings["hooks"]["PreToolUse"][0]["matcher"], "*");
    assert_eq!(
        settings["hooks"].as_object().unwrap().len(),
        hangar_mcp::hook::HOOK_EVENTS.len()
    );
    assert_eq!(x.args.len(), 4);
    assert!(x.files.is_empty());
    assert!(
        !x.args.iter().any(|a| a.contains("ab12")),
        "no secret in argv"
    );
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
                "mcp_servers.hangar={{command=\"{EXE}\",args=[\"mcp-bridge\"],env_vars=[\"HANGAR_MCP_SESSION\",\"HANGAR_MCP_SOCKET\"],tool_timeout_sec=1800}}"
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
