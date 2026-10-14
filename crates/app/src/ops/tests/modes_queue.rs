use super::*;

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
