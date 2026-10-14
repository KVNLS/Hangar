use super::*;

fn parse(name: &str, args: Value) -> Result<Tool, ParseError> {
    Tool::parse(name, Some(&args))
}

fn message(r: Result<Tool, ParseError>) -> String {
    match r {
        Err(ParseError::InvalidArguments { message, .. }) => message,
        other => panic!("expected invalid arguments, got {other:?}"),
    }
}

#[test]
fn orchestration_tools_are_listed_only_when_on() {
    let names = |on| -> Vec<String> {
        definitions(on)
            .iter()
            .map(|d| d["name"].as_str().unwrap().to_owned())
            .collect()
    };
    assert_eq!(names(false), TOOL_NAMES[..10]);
    assert_eq!(names(true), TOOL_NAMES);
    for name in TOOL_NAMES {
        assert_eq!(
            Tool::is_orchestration_name(name),
            ORCHESTRATION_TOOLS.contains(&name)
        );
    }
}

#[test]
fn parses_orchestration_tools() {
    assert_eq!(
        parse("workspaces_list", json!({})),
        Ok(Tool::WorkspacesList)
    );
    assert_eq!(
        parse("workspace_create", json!({"task": "Fix login"})),
        Ok(Tool::WorkspaceCreate {
            task: "Fix login".into(),
            name: None,
            agent: None
        })
    );
    assert_eq!(
        parse(
            "workspace_create",
            json!({"task": "Fix", "name": " ", "agent": " codex-acp "})
        ),
        Ok(Tool::WorkspaceCreate {
            task: "Fix".into(),
            name: None,
            agent: Some("codex-acp".into())
        })
    );
    assert_eq!(
        parse(
            "agent_prompt",
            json!({"workspace": " Login ", "text": "go on"})
        ),
        Ok(Tool::AgentPrompt {
            workspace: "Login".into(),
            text: "go on".into()
        })
    );
    assert_eq!(
        parse("agent_transcript", json!({"workspace": "Login"})),
        Ok(Tool::AgentTranscript {
            workspace: "Login".into(),
            last: DEFAULT_TRANSCRIPT_MESSAGES
        })
    );
    assert_eq!(
        parse(
            "agent_transcript",
            json!({"workspace": "Login", "last": 50})
        ),
        Ok(Tool::AgentTranscript {
            workspace: "Login".into(),
            last: 50
        })
    );
    assert_eq!(
        message(parse("workspace_create", json!({"task": "  "}))),
        "\"task\" must not be empty"
    );
    assert_eq!(
        message(parse("workspace_create", json!({}))),
        "missing required string \"task\""
    );
    assert_eq!(
        message(parse("workspace_create", json!({"task": "x", "agent": ""}))),
        "\"agent\" must not be empty"
    );
    assert_eq!(
        message(parse(
            "agent_prompt",
            json!({"workspace": "x", "text": " "})
        )),
        "\"text\" must not be empty"
    );
    assert_eq!(
        message(parse(
            "agent_prompt",
            json!({"workspace": "", "text": "hi"})
        )),
        "\"workspace\" must not be empty"
    );
    assert!(
        message(parse(
            "agent_transcript",
            json!({"workspace": "x", "last": 51})
        ))
        .contains("between 1 and 50")
    );
    assert!(
        message(parse(
            "agent_transcript",
            json!({"workspace": "x", "last": 0})
        ))
        .contains("between 1 and 50")
    );
    assert_eq!(
        message(parse("workspaces_list", json!({"all": true}))),
        "unknown argument(s) all; this tool takes no arguments"
    );
}

#[test]
fn every_definition_parses_its_name() {
    let defs = definitions(true);
    assert_eq!(defs.len(), TOOL_NAMES.len());
    for (def, name) in defs.iter().zip(TOOL_NAMES) {
        assert_eq!(def["name"], name);
        let props = def["inputSchema"]["properties"]
            .as_object()
            .map(|p| p.len())
            .unwrap_or(0);
        assert_eq!(props, allowed_args(name).len(), "{name}");
    }
}

#[test]
fn parses_happy_paths() {
    assert_eq!(Tool::parse("device_info", None), Ok(Tool::DeviceInfo));
    assert_eq!(parse("screenshot", json!({})), Ok(Tool::Screenshot));
    assert_eq!(
        parse("tap", json!({"x": 10, "y": 20.5})),
        Ok(Tool::Tap { x: 10.0, y: 20.5 })
    );
    assert_eq!(
        parse(
            "swipe",
            json!({"from_x": 1, "from_y": 2, "to_x": 3, "to_y": 4})
        ),
        Ok(Tool::Swipe {
            from_x: 1.0,
            from_y: 2.0,
            to_x: 3.0,
            to_y: 4.0,
            duration_ms: DEFAULT_SWIPE_MS
        })
    );
    assert_eq!(
        parse(
            "swipe",
            json!({"from_x": 1, "from_y": 2, "to_x": 3, "to_y": 4, "duration_ms": 500.0})
        )
        .map(|t| matches!(
            t,
            Tool::Swipe {
                duration_ms: 500,
                ..
            }
        )),
        Ok(true)
    );
    assert_eq!(
        parse("type_text", json!({"text": "hé"})),
        Ok(Tool::TypeText { text: "hé".into() })
    );
    assert_eq!(
        parse("press", json!({"button": "volume_up"})),
        Ok(Tool::Press(Press::Button(Button::VolumeUp)))
    );
    assert_eq!(
        parse("press", json!({"key": "arrow_left"})),
        Ok(Tool::Press(Press::Key(Key::ArrowLeft)))
    );
    assert_eq!(
        parse("logs", json!({})),
        Ok(Tool::Logs {
            lines: 200,
            min_level: None,
            process: None
        })
    );
    assert_eq!(
        parse(
            "logs",
            json!({"lines": 2000, "level": "warning", "process": "MyApp"})
        ),
        Ok(Tool::Logs {
            lines: 2000,
            min_level: Some(LogLevel::Warn),
            process: Some("MyApp".into())
        })
    );
    assert_eq!(parse("build_and_run", json!({})), Ok(Tool::BuildAndRun));
    assert_eq!(
        parse("launch_app", json!({"app_id": "com.example"})),
        Ok(Tool::LaunchApp {
            app_id: "com.example".into()
        })
    );
}

#[test]
fn press_maps_to_device_input() {
    assert_eq!(Press::Button(Button::Home).input(), Input::Home);
    assert_eq!(Press::Key(Key::Enter).input(), Input::Key(Key::Enter));
}

#[test]
fn reports_precise_validation_errors() {
    assert_eq!(
        Tool::parse("nope", None),
        Err(ParseError::UnknownTool("nope".into()))
    );
    assert_eq!(
        message(parse("tap", json!({"x": 1}))),
        "missing required number \"y\""
    );
    assert_eq!(
        message(parse("tap", json!({"x": "1", "y": 2}))),
        "\"x\" must be a number, got a string"
    );
    assert!(message(parse("tap", json!({"x": -1, "y": 2}))).contains("non-negative"));
    assert_eq!(
        message(parse("tap", json!({"x": 1, "y": 2, "z": 3}))),
        "unknown argument(s) z; expected only x, y"
    );
    assert_eq!(
        message(parse("screenshot", json!({"scale": 2}))),
        "unknown argument(s) scale; this tool takes no arguments"
    );
    assert_eq!(
        message(Tool::parse("tap", Some(&json!([1, 2])))),
        "arguments must be an object, got an array"
    );
    assert!(
        message(parse(
            "swipe",
            json!({"from_x": 1, "from_y": 2, "to_x": 3, "to_y": 4, "duration_ms": 1.5})
        ))
        .contains("integer between 0 and 10000")
    );
    assert_eq!(
        message(parse("press", json!({"button": "power"}))),
        "\"button\" must be one of home, back, lock, volume_up, volume_down, app_switch, got \"power\""
    );
    assert_eq!(
        message(parse("press", json!({"button": "home", "key": "enter"}))),
        "pass either \"button\" or \"key\", not both"
    );
    assert_eq!(
        message(parse("press", json!({}))),
        "missing \"button\" or \"key\""
    );
    assert!(message(parse("logs", json!({"lines": 2001}))).contains("between 1 and 2000"));
    assert!(message(parse("logs", json!({"lines": 0}))).contains("between 1 and 2000"));
    assert!(message(parse("logs", json!({"level": "warn"}))).contains("got \"warn\""));
    assert_eq!(
        message(parse("type_text", json!({"text": ""}))),
        "\"text\" must not be empty"
    );
    assert_eq!(
        message(parse("launch_app", json!({}))),
        "missing required string \"app_id\""
    );
}

#[test]
fn long_texts_are_refused() {
    let long = |n: usize| "a".repeat(n + 1);
    assert!(
        message(parse(
            "type_text",
            json!({"text": long(MAX_TYPE_TEXT_CHARS)})
        ))
        .contains("at most 10000 characters")
    );
    assert!(
        parse(
            "type_text",
            json!({"text": "a".repeat(MAX_TYPE_TEXT_CHARS)})
        )
        .is_ok()
    );
    assert!(
        message(parse(
            "agent_prompt",
            json!({"workspace": "w", "text": long(MAX_PROMPT_CHARS)})
        ))
        .contains("\"text\" must have at most")
    );
    assert!(
        message(parse(
            "workspace_create",
            json!({"task": long(MAX_PROMPT_CHARS)})
        ))
        .contains("\"task\" must have at most")
    );
    assert!(
        message(parse(
            "workspace_create",
            json!({"task": "t", "name": long(MAX_NAME_CHARS)})
        ))
        .contains("\"name\" must have at most")
    );
}

#[test]
fn screenshot_caption_maps_pixels_to_points() {
    let shot = Screenshot {
        png: vec![1, 2, 3],
        width: 512,
        height: 1024,
    };
    let out = ToolOutput::screenshot(shot, 393.0, 852.0);
    assert_eq!(out.content[0], Content::Png(vec![1, 2, 3]));
    let Content::Text(caption) = &out.content[1] else {
        panic!("expected caption");
    };
    assert!(caption.contains("512x1024 px"), "{caption}");
    assert!(caption.contains("393 x 852 points"), "{caption}");
}

#[test]
fn ask_user_arguments() {
    let ok = Tool::parse(
        "ask_user",
        Some(&json!({ "question": "  Which API? ", "choices": ["REST", " GraphQL "] })),
    )
    .unwrap();
    assert_eq!(
        ok,
        Tool::AskUser {
            question: "Which API?".into(),
            choices: vec!["REST".into(), "GraphQL".into()],
        }
    );
    let plain = Tool::parse("ask_user", Some(&json!({ "question": "Go?" }))).unwrap();
    assert_eq!(
        plain,
        Tool::AskUser {
            question: "Go?".into(),
            choices: vec![],
        }
    );
    for bad in [
        json!({}),
        json!({ "question": " " }),
        json!({ "question": "x".repeat(MAX_QUESTION_CHARS + 1) }),
        json!({ "question": "q", "choices": "yes" }),
        json!({ "question": "q", "choices": [""] }),
        json!({ "question": "q", "choices": [1] }),
        json!({ "question": "q", "choices": ["a", "b", "c", "d", "e", "f", "g"] }),
        json!({ "question": "q", "other": 1 }),
    ] {
        assert!(
            matches!(
                Tool::parse("ask_user", Some(&bad)),
                Err(ParseError::InvalidArguments { .. })
            ),
            "{bad}"
        );
    }
    let out = ToolOutput::answer(&Answer {
        text: "REST".into(),
        choice: Some(0),
    });
    assert_eq!(
        out.content,
        [Content::Text(r#"{"answer":"REST","choice":0}"#.into())]
    );
}
