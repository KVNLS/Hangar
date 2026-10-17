use super::fixtures::fixture;
use super::*;
use hangar_core::{SessionStatus, Setting};
use hangar_store_sqlite::Store;
use hangar_terminal::ExitInfo;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

#[test]
fn session_runs_records_and_stale_sessions_are_failed_on_load() {
    let f = fixture();
    let ws = f.workspace("run");
    let store = &f.store;

    let adapter =
        hangar_agents::PtyAdapter::new("sh", "sh", "/bin/sh").with_args(["-c", "pwd; exit 3"]);
    let started = start_session_with(
        store,
        &adapter,
        &ws,
        None,
        Arc::new(|| {}),
        &AgentExtras::default(),
    )
    .unwrap();
    let exit = started
        .handle
        .session()
        .wait_for_exit(Duration::from_secs(10))
        .unwrap();
    assert_eq!(exit_status(&exit), SessionStatus::Exited { code: 3 });
    let row = store.get_session(started.session.id).unwrap();
    assert_eq!(row.status, SessionStatus::Running);

    load(store).unwrap();
    let row = store.get_session(started.session.id).unwrap();
    assert!(matches!(row.status, SessionStatus::Failed { .. }));
    assert!(row.ended_at.is_some());
}

#[test]
fn saved_conversations_load_per_workspace() {
    let f = fixture();
    let ws = f.workspace("chat");
    let store = &f.store;
    let chat = SavedChat {
        agent: "claude-acp".into(),
        session: "s-1".into(),
    };
    store
        .set_setting(&setting_keys::chat(ws.id), &chat)
        .unwrap();
    let loaded = load(store).unwrap();
    assert_eq!(loaded.chats.get(&ws.id), Some(&chat));
    assert!(loaded.workspace_agents.is_empty());
    save_workspace_agent(store, ws.id, "codex-acp").unwrap();
    let loaded = load(store).unwrap();
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
    let f = fixture();
    let ws = f.workspace("x");
    let store = &f.store;
    let Some(agents::AgentPreset::Pty(adapter)) = agents::preset("claude") else {
        panic!("claude is a terminal preset");
    };
    let Err(err) = start_session_with(
        store,
        &adapter,
        &ws,
        Some("/nonexistent"),
        Arc::new(|| {}),
        &AgentExtras::default(),
    ) else {
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
    let data = tempfile::tempdir().unwrap();
    let f = fixture();
    let ws = f.workspace("extras");
    let store = &f.store;

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
    let started = start_session_with(store, &adapter, &ws, None, Arc::new(|| {}), &extra).unwrap();
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
    let err = start_session_with(store, &adapter, &ws, None, Arc::new(|| {}), &inside)
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

#[test]
fn settings_are_loaded_and_bad_ones_reported() {
    let f = fixture();
    let loaded = load(&f.store).unwrap();
    assert!(loaded.layout.is_none() && loaded.agent.is_none());
    assert!(loaded.settings_error.is_none());

    let layout = PaneLayout {
        panes: vec![Some(WorkspaceId::new()), None],
        focused: 1,
        columns: vec![1, 1],
        sizes: vec![0.4, 0.6],
        rows: vec![Vec::new(), Vec::new()],
        split: None,
        panel_width: Some(480.0),
        sidebar_width: Some(300.0),
    };
    save_layout(&f.store, &layout).unwrap();
    save_agent(&f.store, "codex").unwrap();
    let loaded = load(&f.store).unwrap();
    assert_eq!(loaded.layout, Some(layout));
    assert_eq!(loaded.agent.as_deref(), Some("codex"));
    assert!(loaded.changes.is_none());

    let changes = ChangesLayout { open: true };
    save_changes(&f.store, &changes).unwrap();
    assert_eq!(load(&f.store).unwrap().changes, Some(changes));

    f.store.set_setting(&Setting::new("layout"), &42).unwrap();
    let loaded = load(&f.store).unwrap();
    assert!(loaded.layout.is_none());
    assert!(loaded.settings_error.is_some());
    assert_eq!(loaded.entries.len(), 1);
}

#[test]
fn orchestration_round_trips() {
    let f = fixture();
    assert_eq!(load(&f.store).unwrap().orchestration, None);
    save_orchestration(&f.store, true).unwrap();
    assert_eq!(load(&f.store).unwrap().orchestration, Some(true));
    save_orchestration(&f.store, false).unwrap();
    assert_eq!(load(&f.store).unwrap().orchestration, Some(false));
}

#[test]
fn notify_attention_round_trips() {
    let f = fixture();
    assert_eq!(load(&f.store).unwrap().notify_attention, None);
    save_notify_attention(&f.store, false).unwrap();
    assert_eq!(load(&f.store).unwrap().notify_attention, Some(false));
    save_notify_attention(&f.store, true).unwrap();
    assert_eq!(load(&f.store).unwrap().notify_attention, Some(true));
}

#[test]
fn notify_finished_round_trips() {
    let f = fixture();
    assert_eq!(load(&f.store).unwrap().notify_finished, None);
    save_notify_finished(&f.store, false).unwrap();
    assert_eq!(load(&f.store).unwrap().notify_finished, Some(false));
}

#[test]
fn device_settings_round_trip() {
    use crate::device_panel::{DeviceKey, Platform};

    let f = fixture();
    assert!(load(&f.store).unwrap().device.is_none());
    let device = DeviceLayout {
        open: true,
        selected: Some(DeviceKey {
            platform: Platform::Ios,
            id: "E33FE595-67AD-470C-AD94-A38EAC9D0F38".into(),
        }),
        stats: true,
        logs: true,
        log_level: Some("error".into()),
    };
    save_device(&f.store, &device).unwrap();
    let loaded = load(&f.store).unwrap();
    assert_eq!(loaded.device, Some(device.clone()));
    assert!(loaded.settings_error.is_none());

    let android = DeviceLayout {
        selected: Some(DeviceKey {
            platform: Platform::Android,
            id: "emulator-5554".into(),
        }),
        ..device
    };
    save_device(&f.store, &android).unwrap();
    assert_eq!(load(&f.store).unwrap().device, Some(android));

    // A value saved before Android existed (platform written as a plain string).
    #[derive(serde::Serialize)]
    struct Saved {
        open: bool,
        width: f32,
        selected: SavedKey,
        stats: bool,
    }
    #[derive(serde::Serialize)]
    struct SavedKey {
        platform: &'static str,
        id: &'static str,
    }
    f.store
        .set_setting(
            &Setting::new("device_panel"),
            &Saved {
                open: true,
                width: 400.0,
                selected: SavedKey {
                    platform: "ios",
                    id: "U",
                },
                stats: false,
            },
        )
        .unwrap();
    let old = load(&f.store).unwrap().device.unwrap();
    assert_eq!(
        old.selected,
        Some(DeviceKey {
            platform: Platform::Ios,
            id: "U".into()
        })
    );

    // Fields added later default when an older value is read.
    #[derive(serde::Serialize)]
    struct Old {
        open: bool,
        width: f32,
    }
    f.store
        .set_setting(
            &Setting::new("device_panel"),
            &Old {
                open: false,
                width: 300.0,
            },
        )
        .unwrap();
    let old = load(&f.store).unwrap().device.unwrap();
    assert_eq!((old.open, old.selected, old.stats), (false, None, false));
    assert!(!old.logs && old.log_level.is_none());

    // A broken device setting is reported but leaves the other settings alone.
    save_agent(&f.store, "codex").unwrap();
    f.store
        .set_setting(&Setting::new("device_panel"), &"nope")
        .unwrap();
    let loaded = load(&f.store).unwrap();
    assert!(loaded.device.is_none());
    assert!(loaded.settings_error.is_some());
    assert_eq!(loaded.agent.as_deref(), Some("codex"));
}

#[test]
fn approval_level_defaults_to_ask_and_round_trips() {
    let store = Store::open_in_memory().unwrap();
    assert_eq!(approval_level(&store), ApprovalLevel::Ask);
    save_approval_level(&store, ApprovalLevel::FullAuto).unwrap();
    assert_eq!(approval_level(&store), ApprovalLevel::FullAuto);
    store
        .set_setting(&setting_keys::APPROVAL_LEVEL, &"bogus".to_owned())
        .unwrap();
    assert_eq!(approval_level(&store), ApprovalLevel::Ask);
}

#[test]
fn workspace_mode_wins_for_its_agent_only() {
    let mode = |id: &str| SessionMode {
        id: id.into(),
        name: id.into(),
        description: None,
    };
    let claude = [
        mode("default"),
        mode("acceptEdits"),
        mode("bypassPermissions"),
    ];
    let saved = SavedMode {
        agent: "claude-acp".into(),
        mode: "acceptEdits".into(),
    };
    let full = ApprovalLevel::FullAuto;
    assert_eq!(
        start_mode(full, Some(&saved), "claude-acp", &claude),
        Some("acceptEdits")
    );
    assert_eq!(
        start_mode(full, Some(&saved), "codex-acp", &claude),
        Some("bypassPermissions")
    );
    assert_eq!(
        start_mode(full, None, "claude-acp", &claude),
        Some("bypassPermissions")
    );
    // A saved mode the agent no longer offers falls back to the level.
    let gone = SavedMode {
        mode: "dontAsk".into(),
        ..saved
    };
    assert_eq!(
        start_mode(ApprovalLevel::Ask, Some(&gone), "claude-acp", &claude),
        Some("default")
    );
    assert_eq!(
        start_mode(full, None, "opencode-acp", &[mode("build"), mode("plan")]),
        None
    );

    let store = Store::open_in_memory().unwrap();
    let ws = WorkspaceId::new();
    remember_workspace_mode(&store, ws, "claude-acp", "plan").unwrap();
    assert_eq!(
        store
            .get_setting(&setting_keys::workspace_mode(ws))
            .unwrap(),
        Some(SavedMode {
            agent: "claude-acp".into(),
            mode: "plan".into()
        })
    );
}

#[test]
fn queued_starts_are_saved_and_loaded_in_order() {
    let store = Store::open_in_memory().unwrap();
    assert!(load(&store).unwrap().agent_queue.is_empty());
    let queue = vec![
        (
            WorkspaceId::new(),
            StartRequest {
                agent: "claude-acp".into(),
                resume: false,
                input: Some("Fix the login".into()),
            },
        ),
        (
            WorkspaceId::new(),
            StartRequest {
                agent: "codex".into(),
                resume: true,
                input: None,
            },
        ),
    ];
    save_agent_queue(&store, &queue).unwrap();
    assert_eq!(load(&store).unwrap().agent_queue, queue);
}

#[test]
fn coauthor_trailer_round_trips() {
    let f = fixture();
    assert_eq!(coauthor_trailer(&f.store).unwrap(), None);
    save_coauthor_trailer(&f.store, false).unwrap();
    assert_eq!(coauthor_trailer(&f.store).unwrap(), Some(false));
    save_coauthor_trailer(&f.store, true).unwrap();
    assert_eq!(coauthor_trailer(&f.store).unwrap(), Some(true));
}
