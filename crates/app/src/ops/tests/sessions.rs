use super::*;

#[test]
fn session_runs_records_and_stale_sessions_are_failed_on_load() {
    let repo = repo();
    let root = tempfile::tempdir().unwrap();
    let store = Store::open_in_memory().unwrap();
    let project = add_project(&store, repo.path()).unwrap();
    let ws = create_workspace(&store, &project, "run", root.path()).unwrap();

    let adapter =
        hangar_agents::PtyAdapter::new("sh", "sh", "/bin/sh").with_args(["-c", "pwd; exit 3"]);
    let started = start_session(&store, &adapter, &ws, None, Arc::new(|| {})).unwrap();
    let exit = started
        .handle
        .session()
        .wait_for_exit(Duration::from_secs(10))
        .unwrap();
    assert_eq!(exit_status(&exit), SessionStatus::Exited { code: 3 });
    let row = store.get_session(started.session.id).unwrap();
    assert_eq!(row.status, SessionStatus::Running);

    load(&store).unwrap();
    let row = store.get_session(started.session.id).unwrap();
    assert!(matches!(row.status, SessionStatus::Failed { .. }));
    assert!(row.ended_at.is_some());
}

#[test]
fn saved_conversations_load_per_workspace() {
    let repo = repo();
    let root = tempfile::tempdir().unwrap();
    let store = Store::open_in_memory().unwrap();
    let project = add_project(&store, repo.path()).unwrap();
    let ws = create_workspace(&store, &project, "chat", root.path()).unwrap();
    let chat = SavedChat {
        agent: "claude-acp".into(),
        session: "s-1".into(),
    };
    store
        .set_setting(&setting_keys::chat(ws.id), &chat)
        .unwrap();
    let loaded = load(&store).unwrap();
    assert_eq!(loaded.chats.get(&ws.id), Some(&chat));
    assert!(loaded.workspace_agents.is_empty());
    save_workspace_agent(&store, ws.id, "codex-acp").unwrap();
    let loaded = load(&store).unwrap();
    assert_eq!(
        loaded.workspace_agents.get(&ws.id).map(String::as_str),
        Some("codex-acp")
    );
}

#[test]
fn agent_config_choices_replace_and_keep_order() {
    let store = Store::open_in_memory().unwrap();
    assert!(saved_agent_config(&store, "codex-acp").is_empty());
    remember_agent_config(&store, "codex-acp", "model", "gpt-5.4").unwrap();
    remember_agent_config(&store, "codex-acp", "reasoning_effort", "high").unwrap();
    remember_agent_config(&store, "codex-acp", "model", "gpt-5.5").unwrap();
    remember_agent_config(&store, "claude-acp", "model", "opus").unwrap();
    let pair = |a: &str, b: &str| (a.to_owned(), b.to_owned());
    assert_eq!(
        saved_agent_config(&store, "codex-acp"),
        [pair("reasoning_effort", "high"), pair("model", "gpt-5.5")]
    );
    assert_eq!(
        saved_agent_config(&store, "claude-acp"),
        [pair("model", "opus")]
    );
}

#[test]
fn failed_spawn_is_recorded() {
    let repo = repo();
    let root = tempfile::tempdir().unwrap();
    let store = Store::open_in_memory().unwrap();
    let project = add_project(&store, repo.path()).unwrap();
    let ws = create_workspace(&store, &project, "x", root.path()).unwrap();
    let Some(agents::AgentPreset::Pty(adapter)) = agents::preset("claude") else {
        panic!("claude is a terminal preset");
    };
    let Err(err) = start_session(&store, &adapter, &ws, Some("/nonexistent"), Arc::new(|| {}))
    else {
        panic!("claude is not on /nonexistent");
    };
    let cause = "claude: `claude` was not found on PATH";
    assert_eq!(
        format!("{err:#}"),
        format!("failed to start Claude Code: {cause}")
    );
    let sessions = store.list_sessions(ws.id).unwrap();
    assert_eq!(sessions.len(), 1);
    assert_eq!(
        sessions[0].status,
        SessionStatus::Failed {
            message: cause.to_owned()
        }
    );
}

#[test]
fn session_extras_write_files_and_reach_the_agent() {
    use std::os::unix::fs::PermissionsExt as _;
    let repo = repo();
    let root = tempfile::tempdir().unwrap();
    let data = tempfile::tempdir().unwrap();
    let store = Store::open_in_memory().unwrap();
    let project = add_project(&store, repo.path()).unwrap();
    let ws = create_workspace(&store, &project, "extras", root.path()).unwrap();

    let config = data.path().join("mcp/ws.json");
    let out = data.path().join("out.txt");
    let adapter = hangar_agents::PtyAdapter::new("sh", "sh", "/bin/sh").with_args([
        "-c",
        r#"printf '%s|%s|%s' "$HANGAR_X" "$1" "$(cat "$CONFIG")" > "$OUT""#,
        "sh",
    ]);
    let extra = AgentExtras {
        mcp_servers: Vec::new(),
        args: vec!["--flag".into()],
        env: vec![
            ("HANGAR_X".into(), "x1".into()),
            ("CONFIG".into(), config.display().to_string()),
            ("OUT".into(), out.display().to_string()),
        ],
        files: vec![(config.clone(), "{}".into())],
        prompt: Some("Fix the build".into()),
    };
    let started = start_session_with(&store, &adapter, &ws, None, Arc::new(|| {}), &extra).unwrap();
    let exit = started
        .handle
        .session()
        .wait_for_exit(Duration::from_secs(10))
        .unwrap();
    assert_eq!(exit_status(&exit), SessionStatus::Exited { code: 0 });
    assert_eq!(std::fs::read_to_string(&out).unwrap(), "x1|--flag|{}");
    let mode = |p: &Path| std::fs::metadata(p).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode(&config), 0o600);
    assert_eq!(mode(config.parent().unwrap()), 0o700);
    assert_eq!(
        started.session.args.last().map(String::as_str),
        Some("--flag")
    );
    assert_eq!(
        store.list_sessions(ws.id).unwrap()[0].prompt.as_deref(),
        Some("Fix the build")
    );

    let inside = AgentExtras {
        files: vec![(ws.worktree_path.join(".mcp.json"), "{}".into())],
        ..AgentExtras::default()
    };
    let err = start_session_with(&store, &adapter, &ws, None, Arc::new(|| {}), &inside)
        .err()
        .unwrap();
    assert!(
        format!("{err:#}").contains("outside the worktree"),
        "{err:#}"
    );
    assert!(!ws.worktree_path.join(".mcp.json").exists());
    let sessions = store.list_sessions(ws.id).unwrap();
    assert!(
        sessions
            .iter()
            .any(|s| matches!(s.status, SessionStatus::Failed { .. }))
    );
}

#[test]
fn exit_status_mapping() {
    let ok = |code, signal: Option<&str>| {
        Ok(ExitInfo {
            code,
            signal: signal.map(str::to_owned),
        })
    };
    assert_eq!(exit_status(&ok(0, None)), SessionStatus::Exited { code: 0 });
    assert!(matches!(
        exit_status(&ok(1, Some("SIGKILL"))),
        SessionStatus::Failed { .. }
    ));
    assert!(matches!(
        exit_status(&Err("boom".into())),
        SessionStatus::Failed { .. }
    ));
}
