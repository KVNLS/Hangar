use super::*;

#[test]
fn parses_turn_usage_as_claude_agent_acp_sends_it() {
    let result = json!({
        "stopReason": "end_turn",
        "usage": {"inputTokens": 12, "outputTokens": 340, "cachedReadTokens": 21000,
                  "cachedWriteTokens": 800, "totalTokens": 22152},
        "_meta": {"quota": {
            "token_count": {"totalTokens": 22152},
            "model_usage": [
                {"model": "claude-sonnet-4-5", "token_count": {"totalTokens": 22152, "inputTokens": 12}},
                {"model": "claude-haiku-4-5", "token_count": {"totalTokens": 900}},
                {"model": "", "token_count": {"totalTokens": 5}},
                {"model": "broken"}
            ]
        }}
    });
    assert_eq!(
        parse_turn_usage(&result),
        Some(TurnUsage {
            tokens: Some(TurnTokens {
                input: 12,
                output: 340,
                cache_read: 21_000,
                cache_write: 800,
            }),
            models: vec![
                ("claude-sonnet-4-5".into(), 22_152),
                ("claude-haiku-4-5".into(), 900),
            ],
        })
    );
    // An agent that only reports totals per model, or nothing at all.
    let only_models = json!({"_meta": {"quota": {"model_usage": [
        {"model": "gpt-5-codex", "token_count": {"totalTokens": 10}}
    ]}}});
    assert_eq!(parse_turn_usage(&only_models).unwrap().tokens, None);
    assert_eq!(parse_turn_usage(&json!({"stopReason": "end_turn"})), None);
    assert_eq!(
        parse_turn_usage(&json!({"usage": {"inputTokens": "12"}})),
        None
    );
}

#[test]
fn parses_chunks_and_non_text_blocks() {
    let u =
        json!({"sessionUpdate": "agent_message_chunk", "content": {"type": "text", "text": "hi"}});
    assert_eq!(
        parse_update(&u),
        Some(SessionUpdate::AgentMessage {
            text: "hi".into(),
            parent: None
        })
    );
    let u = json!({"sessionUpdate": "agent_thought_chunk", "content": {"type": "image", "data": "", "mimeType": "image/png"}});
    assert_eq!(
        parse_update(&u),
        Some(SessionUpdate::AgentThought {
            text: "[image]".into(),
            parent: None
        })
    );
    let link = json!({"type": "resource_link", "uri": "file:///a.rs", "name": "a.rs"});
    assert_eq!(content_text(&link), "[a.rs](file:///a.rs)");
}

#[test]
fn tool_call_then_update() {
    let u = json!({
        "sessionUpdate": "tool_call", "toolCallId": "t1", "title": "Read a.rs", "kind": "read",
        "status": "pending", "locations": [{"path": "/w/a.rs", "line": 3}]
    });
    let Some(SessionUpdate::ToolCall(mut call)) = parse_update(&u) else {
        panic!("not a tool call");
    };
    assert_eq!(call.kind, "read");
    assert_eq!(
        call.locations,
        vec![Location {
            path: "/w/a.rs".into(),
            line: Some(3)
        }]
    );
    let u = json!({
        "sessionUpdate": "tool_call_update", "toolCallId": "t1", "status": "completed",
        "content": [
            {"type": "content", "content": {"type": "text", "text": "ok"}},
            {"type": "diff", "path": "/w/a.rs", "oldText": "a", "newText": "b"}
        ]
    });
    let Some(SessionUpdate::ToolCallUpdate(update)) = parse_update(&u) else {
        panic!("not an update");
    };
    call.apply(&update);
    assert_eq!(call.status, ToolStatus::Completed);
    assert_eq!(call.title, "Read a.rs");
    assert_eq!(call.content.len(), 2);
    assert_eq!(
        call.content[1],
        ToolContent::Diff {
            path: "/w/a.rs".into(),
            old_text: Some("a".into()),
            new_text: "b".into()
        }
    );
}

#[test]
fn subagent_metadata() {
    let u = json!({
        "sessionUpdate": "tool_call", "toolCallId": "a1", "title": "Migrate CartStore",
        "kind": "think", "status": "in_progress",
        "rawInput": {"description": "Migrate CartStore", "subagent_type": "general-purpose",
                     "prompt": "Move CartStore to @Observable"},
        "_meta": {"claudeCode": {"toolName": "Agent"}}
    });
    let Some(SessionUpdate::ToolCall(launch)) = parse_update(&u) else {
        panic!("not a tool call");
    };
    assert!(launch.is_subagent());
    assert_eq!(launch.subagent_type.as_deref(), Some("general-purpose"));
    assert_eq!(launch.parent, None);
    assert_eq!(
        launch.prompt.as_deref(),
        Some("Move CartStore to @Observable")
    );

    let u = json!({
        "sessionUpdate": "tool_call", "toolCallId": "c1", "title": "Read a.rs", "kind": "read",
        "_meta": {"claudeCode": {"toolName": "Read", "parentToolUseId": "a1"}}
    });
    let Some(SessionUpdate::ToolCall(mut child)) = parse_update(&u) else {
        panic!("not a tool call");
    };
    assert!(!child.is_subagent());
    assert_eq!(child.parent.as_deref(), Some("a1"));

    let u = json!({"sessionUpdate": "tool_call_update", "toolCallId": "c1", "status": "completed"});
    let Some(SessionUpdate::ToolCallUpdate(update)) = parse_update(&u) else {
        panic!("not an update");
    };
    child.apply(&update);
    assert_eq!(child.parent.as_deref(), Some("a1"));
    assert_eq!(child.tool_name.as_deref(), Some("Read"));

    let u = json!({
        "sessionUpdate": "agent_message_chunk", "content": {"type": "text", "text": "done"},
        "_meta": {"claudeCode": {"parentToolUseId": "a1"}}
    });
    assert_eq!(
        parse_update(&u),
        Some(SessionUpdate::AgentMessage {
            text: "done".into(),
            parent: Some("a1".into())
        })
    );
}

#[test]
fn plan_and_unknown_update() {
    let u = json!({"sessionUpdate": "plan", "entries": [
        {"content": "a", "priority": "high", "status": "completed"},
        {"content": "b", "priority": "low", "status": "in_progress"}
    ]});
    let Some(SessionUpdate::Plan(plan)) = parse_update(&u) else {
        panic!("not a plan");
    };
    assert_eq!(plan[0].status, PlanStatus::Completed);
    assert_eq!(plan[1].priority, "low");
    let u = json!({"sessionUpdate": "session_info_update", "title": "t"});
    assert_eq!(
        parse_update(&u),
        Some(SessionUpdate::Other {
            kind: "session_info_update".into()
        })
    );
}

#[test]
fn usage_updates_from_each_agent() {
    // claude-agent-acp 0.85 at the end of a turn: running SDK cost, model in `_meta`.
    let u = json!({"sessionUpdate": "usage_update", "used": 84000, "size": 200000,
        "cost": {"amount": 0.3114, "currency": "USD"},
        "_meta": {"_claude/model": "claude-opus-5-5"}});
    assert_eq!(
        parse_update(&u),
        Some(SessionUpdate::Usage(Usage {
            used_tokens: Some(84_000),
            context_size: Some(200_000),
            cost: Some(Cost {
                amount: 0.3114,
                currency: "USD".into()
            }),
        }))
    );
    // claude-agent-acp mid-stream and codex-acp 0.16: no cost.
    let u = json!({"sessionUpdate": "usage_update", "used": 12, "size": 258400});
    let Some(SessionUpdate::Usage(usage)) = parse_update(&u) else {
        panic!("usage expected");
    };
    assert_eq!(usage.cost, None);
    assert_eq!(usage.context_size, Some(258_400));
    // OpenCode 1.18: the sum of its messages' costs, here an integer.
    let u = json!({"sessionUpdate": "usage_update", "used": 9000, "size": 128000,
        "cost": {"amount": 0, "currency": "USD"}});
    let Some(SessionUpdate::Usage(usage)) = parse_update(&u) else {
        panic!("usage expected");
    };
    assert_eq!(usage.cost.map(|c| c.amount), Some(0.0));
    // Missing or malformed fields are dropped, not guessed.
    let u = json!({"sessionUpdate": "usage_update", "used": 3, "size": -1,
        "cost": {"amount": 1.5}});
    assert_eq!(
        parse_update(&u),
        Some(SessionUpdate::Usage(Usage {
            used_tokens: Some(3),
            ..Usage::default()
        }))
    );
}

#[test]
fn permission_options_and_results() {
    let p = json!({
        "sessionId": "s",
        "toolCall": {"toolCallId": "t1", "title": "Edit a.rs"},
        "options": [
            {"optionId": "y", "name": "Allow", "kind": "allow_once"},
            {"optionId": "n", "name": "Reject", "kind": "reject_once"},
            {"optionId": "?", "name": "Odd", "kind": "maybe"}
        ]
    });
    let (call, options) = parse_permission(&p).expect("parses");
    assert_eq!(call.title.as_deref(), Some("Edit a.rs"));
    assert_eq!(options.len(), 2);
    assert!(options[0].kind.is_allow());
    assert_eq!(
        permission_result(Some("y")),
        json!({"outcome": {"outcome": "selected", "optionId": "y"}})
    );
    assert_eq!(
        permission_result(None),
        json!({"outcome": {"outcome": "cancelled"}})
    );
}

#[test]
fn modes_from_session_new() {
    let result = json!({"sessionId": "s", "modes": {"currentModeId": "default", "availableModes": [
        {"id": "default", "name": "Manual"}, {"id": "plan", "name": "Plan"}
    ]}});
    let (current, modes) = parse_modes(&result);
    assert_eq!(current.as_deref(), Some("default"));
    assert_eq!(modes[0].name, "Manual");
    assert_eq!(parse_modes(&json!({"sessionId": "s"})), (None, Vec::new()));
}

#[test]
fn modes_from_config_options() {
    // OpenCode 1.18: no `modes`, a `mode` select among the config options.
    let options = json!([
        {"id": "model", "category": "model", "type": "select", "currentValue": "m", "options": [{"value": "m", "name": "M"}]},
        {"id": "mode", "name": "Session Mode", "category": "mode", "type": "select", "currentValue": "build", "options": [
            {"value": "build", "name": "build", "description": "The default agent."},
            {"value": "plan", "name": "plan"}
        ]}
    ]);
    let (current, modes) = parse_modes(&json!({"sessionId": "s", "configOptions": options}));
    assert_eq!(current.as_deref(), Some("build"));
    let ids: Vec<&str> = modes.iter().map(|m| m.id.as_str()).collect();
    assert_eq!(ids, ["build", "plan"]);

    let mut changed = options.clone();
    changed[1]["currentValue"] = json!("plan");
    let update = json!({"sessionUpdate": "config_option_update", "configOptions": changed});
    let Some(SessionUpdate::ConfigOptions(list)) = parse_update(&update) else {
        panic!("config options expected");
    };
    assert!(list[1].is_mode() && list[1].current == "plan");
}

#[test]
fn config_options_keep_selects_and_flatten_groups() {
    // Claude Code's effort select, a grouped model select and a non-select entry.
    let options = json!([
        {"id": "effort", "name": "Effort", "category": "thought_level", "type": "select", "currentValue": "high",
         "options": [{"value": "low", "name": "Low"}, {"value": "high", "name": "High"}]},
        {"id": "model", "name": "Model", "category": "model", "type": "select", "currentValue": "b",
         "options": [{"group": "g1", "name": "Fast", "options": [{"value": "a", "name": "A"}]},
                     {"group": "g2", "name": "Smart", "options": [{"value": "b", "name": "B"}]}]},
        {"id": "verbose", "type": "boolean", "currentValue": true},
        {"id": "odd", "type": "select", "options": []}
    ]);
    let list = parse_config_options(&options);
    assert_eq!(list.len(), 2);
    assert_eq!(list[0].current_name(), "High");
    assert_eq!(list[0].category.as_deref(), Some("thought_level"));
    let values: Vec<&str> = list[1].values.iter().map(|v| v.value.as_str()).collect();
    assert_eq!(values, ["a", "b"]);
    assert_eq!(list[1].current_name(), "B");
    assert!(!list[1].is_mode());
    assert_eq!(
        set_config_option_params("s", "model", "a"),
        json!({"sessionId": "s", "configId": "model", "value": "a"})
    );
}

#[test]
fn mcp_servers_use_env_lists() {
    let server = McpServer {
        name: "hangar".into(),
        command: "/h".into(),
        args: vec!["mcp-bridge".into()],
        env: vec![("K".into(), "V".into())],
    };
    let p = new_session_params("/w", &[server]);
    assert_eq!(
        p["mcpServers"][0]["env"],
        json!([{"name": "K", "value": "V"}])
    );
}
