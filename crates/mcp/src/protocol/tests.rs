use super::*;
use crate::tools::Press;
use hangar_core::device::Key;
use std::sync::Mutex;

pub const SESSION: &str = "tok-123";

#[derive(Default)]
pub struct FakeHost {
    pub calls: Mutex<Vec<(String, Tool)>>,
    pub signals: Mutex<Vec<AgentSignal>>,
    pub orchestration: std::sync::atomic::AtomicBool,
}

impl ToolHost for FakeHost {
    fn knows_session(&self, session: &str) -> bool {
        session == SESSION
    }

    fn call(&self, session: &str, tool: Tool) -> Result<ToolOutput, ToolError> {
        if session != SESSION {
            return Err(ToolError::UnknownSession);
        }
        self.calls
            .lock()
            .unwrap()
            .push((session.to_owned(), tool.clone()));
        match tool {
            Tool::Screenshot => Ok(ToolOutput {
                content: vec![
                    Content::Png(b"foo".to_vec()),
                    Content::Text("393 x 852 points".into()),
                ],
            }),
            Tool::LaunchApp { app_id } if app_id == "missing" => {
                Err(ToolError::Failed(format!("{app_id} is not installed")))
            }
            Tool::Press(Press::Key(_)) => Err(ToolError::Unsupported("keys on this device".into())),
            other => Ok(ToolOutput::text(format!("ok {}", other.name()))),
        }
    }

    fn agent_signal(
        &self,
        session: &str,
        signal: AgentSignal,
        _transcript: Option<&str>,
    ) -> Result<(), ToolError> {
        if session != SESSION {
            return Err(ToolError::UnknownSession);
        }
        self.signals.lock().unwrap().push(signal);
        Ok(())
    }

    fn orchestration_enabled(&self, _: &str) -> bool {
        self.orchestration.load(std::sync::atomic::Ordering::SeqCst)
    }
}

#[test]
fn orchestration_tools_need_the_setting() {
    let host = FakeHost::default();
    let names = |host: &FakeHost| -> Vec<String> {
        let r = call(host, request(1, "tools/list", json!({}))).unwrap();
        r["result"]["tools"]
            .as_array()
            .unwrap()
            .iter()
            .map(|t| t["name"].as_str().unwrap().to_owned())
            .collect()
    };
    assert!(!names(&host).iter().any(|n| n == "workspace_create"));
    let init = call(&host, request(2, "initialize", json!({}))).unwrap();
    let instructions = init["result"]["instructions"].as_str().unwrap();
    assert!(!instructions.contains("workspace_create"));
    // Agents running simctl/adb themselves must take the live id, not the launch-time env.
    assert!(instructions.contains("call device_info"));
    assert!(instructions.contains("may be missing or stale"));
    // Off: refused before the arguments are even looked at.
    let r = call(
        &host,
        request(3, "tools/call", json!({ "name": "workspace_create" })),
    )
    .unwrap();
    assert_eq!(r["result"]["isError"], true);
    assert!(
        r["result"]["content"][0]["text"]
            .as_str()
            .unwrap()
            .contains("Let agents create and drive workspaces"),
        "{r}"
    );
    assert!(host.calls.lock().unwrap().is_empty());

    host.orchestration
        .store(true, std::sync::atomic::Ordering::SeqCst);
    let listed = names(&host);
    assert_eq!(
        listed[listed.len() - 4..],
        [
            "workspaces_list",
            "workspace_create",
            "agent_prompt",
            "agent_transcript"
        ]
    );
    let init = call(&host, request(4, "initialize", json!({}))).unwrap();
    assert!(
        init["result"]["instructions"]
            .as_str()
            .unwrap()
            .contains("workspace_create")
    );
    let r = call(
        &host,
        request(
            5,
            "tools/call",
            json!({ "name": "agent_prompt", "arguments": { "workspace": "Login", "text": "go" } }),
        ),
    )
    .unwrap();
    assert_eq!(r["result"]["isError"], false);
    assert_eq!(
        host.calls.lock().unwrap()[0].1,
        Tool::AgentPrompt {
            workspace: "Login".into(),
            text: "go".into()
        }
    );
}

#[test]
fn agent_signals_reach_the_host() {
    let host = FakeHost::default();
    let ok = call(
        &host,
        request(1, AGENT_SIGNAL_METHOD, json!({ "signal": "approval" })),
    )
    .unwrap();
    assert_eq!(ok["result"], json!({}));
    let bad = call(
        &host,
        request(2, AGENT_SIGNAL_METHOD, json!({ "signal": "done" })),
    )
    .unwrap();
    assert_eq!(bad["error"]["code"], INVALID_PARAMS);
    let stranger = handle(
        &host,
        "other",
        &request(3, AGENT_SIGNAL_METHOD, json!({ "signal": "working" })).to_string(),
    )
    .unwrap();
    assert!(stranger.contains("-32001"), "{stranger}");
    assert_eq!(*host.signals.lock().unwrap(), [AgentSignal::Approval]);
}

fn call(host: &FakeHost, msg: Value) -> Option<Value> {
    handle(host, SESSION, &msg.to_string()).map(|s| serde_json::from_str(&s).unwrap())
}

fn request(id: i64, method: &str, params: Value) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params })
}

#[test]
fn initialize_negotiates_version() {
    let host = FakeHost::default();
    for (asked, expected) in [
        ("2025-06-18", "2025-06-18"),
        ("2025-03-26", "2025-03-26"),
        ("2024-11-05", "2024-11-05"),
        ("2099-01-01", "2025-06-18"),
    ] {
        let r = call(
            &host,
            request(
                1,
                "initialize",
                json!({
                    "protocolVersion": asked,
                    "capabilities": {},
                    "clientInfo": { "name": "test", "version": "1" },
                }),
            ),
        )
        .unwrap();
        assert_eq!(r["id"], 1);
        assert_eq!(r["result"]["protocolVersion"], expected);
        assert_eq!(r["result"]["capabilities"], json!({ "tools": {} }));
        assert_eq!(r["result"]["serverInfo"]["name"], "hangar");
        assert_eq!(
            r["result"]["serverInfo"]["version"],
            env!("CARGO_PKG_VERSION")
        );
    }
    let r = call(&host, request(2, "initialize", json!({}))).unwrap();
    assert_eq!(r["result"]["protocolVersion"], "2025-06-18");
}

#[test]
fn notifications_get_no_response() {
    let host = FakeHost::default();
    assert_eq!(
        call(
            &host,
            json!({ "jsonrpc": "2.0", "method": "notifications/initialized" })
        ),
        None
    );
    assert_eq!(
        call(
            &host,
            json!({ "jsonrpc": "2.0", "method": "tools/call", "params": {"name": "tap"} })
        ),
        None
    );
    assert_eq!(
        call(&host, json!({ "jsonrpc": "2.0", "method": "whatever" })),
        None
    );
    assert!(host.calls.lock().unwrap().is_empty());
    // A client response to a server request is not answered either.
    assert_eq!(
        call(&host, json!({ "jsonrpc": "2.0", "id": 4, "result": {} })),
        None
    );
}

#[test]
fn ping_unknown_method_and_parse_error() {
    let host = FakeHost::default();
    let r = call(&host, request(7, "ping", Value::Null)).unwrap();
    assert_eq!(r, json!({ "jsonrpc": "2.0", "id": 7, "result": {} }));

    let r = call(
        &host,
        json!({ "jsonrpc": "2.0", "id": "a", "method": "resources/list" }),
    )
    .unwrap();
    assert_eq!(r["id"], "a");
    assert_eq!(r["error"]["code"], METHOD_NOT_FOUND);

    let r: Value = serde_json::from_str(&handle(&host, SESSION, "{not json").unwrap()).unwrap();
    assert_eq!(r["id"], Value::Null);
    assert_eq!(r["error"]["code"], PARSE_ERROR);

    let r = call(&host, json!({ "jsonrpc": "2.0", "id": 3, "method": 5 })).unwrap();
    assert_eq!(r["error"]["code"], INVALID_REQUEST);
}

#[test]
fn tools_list_shape() {
    let host = FakeHost::default();
    let r = call(&host, request(1, "tools/list", json!({}))).unwrap();
    let tools = r["result"]["tools"].as_array().unwrap();
    let names: Vec<&str> = tools.iter().map(|t| t["name"].as_str().unwrap()).collect();
    assert_eq!(
        names,
        [
            "device_info",
            "screenshot",
            "tap",
            "swipe",
            "type_text",
            "press",
            "logs",
            "build_and_run",
            "launch_app",
            "ask_user"
        ]
    );
    for t in tools {
        let name = t["name"].as_str().unwrap();
        assert!(
            t["description"].as_str().is_some_and(|d| d.len() > 20),
            "{name}"
        );
        let schema = &t["inputSchema"];
        assert_eq!(schema["type"], "object", "{name}");
        assert!(schema["properties"].is_object(), "{name}");
        assert_eq!(schema["additionalProperties"], false, "{name}");
        for req in schema["required"].as_array().into_iter().flatten() {
            let req = req.as_str().unwrap();
            assert!(schema["properties"].get(req).is_some(), "{name}.{req}");
        }
    }
    let by_name = |n: &str| tools.iter().find(|t| t["name"] == n).unwrap()["inputSchema"].clone();
    assert_eq!(by_name("tap")["required"], json!(["x", "y"]));
    assert_eq!(
        by_name("swipe")["properties"]["duration_ms"]["default"],
        300
    );
    assert_eq!(
        by_name("press")["properties"]["button"]["enum"],
        json!([
            "home",
            "back",
            "lock",
            "volume_up",
            "volume_down",
            "app_switch"
        ])
    );
    assert_eq!(
        by_name("press")["properties"]["key"]["enum"],
        json!([
            "enter",
            "backspace",
            "tab",
            "escape",
            "arrow_up",
            "arrow_down",
            "arrow_left",
            "arrow_right"
        ])
    );
    assert_eq!(
        by_name("logs")["properties"]["level"]["enum"],
        json!(["verbose", "debug", "info", "warning", "error"])
    );
    assert_eq!(by_name("logs")["properties"]["lines"]["maximum"], 2000);
    assert_eq!(by_name("launch_app")["required"], json!(["app_id"]));
}

#[test]
fn tools_call_happy_paths() {
    let host = FakeHost::default();
    let r = call(
        &host,
        request(
            1,
            "tools/call",
            json!({ "name": "tap", "arguments": { "x": 10, "y": 20 } }),
        ),
    )
    .unwrap();
    assert_eq!(
        r["result"],
        json!({ "content": [ { "type": "text", "text": "ok tap" } ], "isError": false })
    );
    let r = call(
        &host,
        request(2, "tools/call", json!({ "name": "screenshot" })),
    )
    .unwrap();
    assert_eq!(
        r["result"]["content"][0],
        json!({ "type": "image", "data": "Zm9v", "mimeType": "image/png" })
    );
    assert_eq!(r["result"]["content"][1]["type"], "text");
    assert_eq!(r["result"]["isError"], false);
    let calls = host.calls.lock().unwrap();
    assert_eq!(
        calls[0],
        (SESSION.to_owned(), Tool::Tap { x: 10.0, y: 20.0 })
    );
    assert_eq!(calls[1].1, Tool::Screenshot);
}

#[test]
fn tool_failures_are_error_results() {
    let host = FakeHost::default();
    let r = call(
        &host,
        request(
            1,
            "tools/call",
            json!({ "name": "launch_app", "arguments": { "app_id": "missing" } }),
        ),
    )
    .unwrap();
    assert_eq!(r["result"]["isError"], true);
    assert_eq!(
        r["result"]["content"][0]["text"],
        "launch_app failed: missing is not installed"
    );

    let r = call(
        &host,
        request(
            2,
            "tools/call",
            json!({ "name": "press", "arguments": { "key": "enter" } }),
        ),
    )
    .unwrap();
    assert_eq!(r["result"]["isError"], true);
    assert_eq!(
        r["result"]["content"][0]["text"],
        "press failed: unsupported: keys on this device"
    );
    assert_eq!(
        host.calls.lock().unwrap()[1].1,
        Tool::Press(Press::Key(Key::Enter))
    );
}

#[test]
fn argument_validation_errors() {
    let host = FakeHost::default();
    let r = call(
        &host,
        request(
            1,
            "tools/call",
            json!({ "name": "tap", "arguments": { "x": 1 } }),
        ),
    )
    .unwrap();
    assert_eq!(r["result"]["isError"], true);
    assert_eq!(
        r["result"]["content"][0]["text"],
        "invalid arguments for tap: missing required number \"y\""
    );
    let r = call(&host, request(2, "tools/call", json!({ "name": "fly" }))).unwrap();
    assert_eq!(r["error"]["code"], INVALID_PARAMS);
    assert_eq!(r["error"]["message"], "unknown tool \"fly\"");
    let r = call(&host, request(3, "tools/call", json!({ "arguments": {} }))).unwrap();
    assert_eq!(r["error"]["code"], INVALID_PARAMS);
    assert!(host.calls.lock().unwrap().is_empty());
}

#[test]
fn unknown_session_is_rejected() {
    let host = FakeHost::default();
    for method in ["initialize", "ping", "tools/list", "tools/call"] {
        let r: Value = serde_json::from_str(
            &handle(
                &host,
                "other",
                &request(1, method, json!({ "name": "screenshot" })).to_string(),
            )
            .unwrap(),
        )
        .unwrap();
        assert_eq!(r["error"]["code"], UNKNOWN_SESSION, "{method}");
    }
    assert_eq!(
        handle(
            &host,
            "other",
            r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#
        ),
        None
    );
    assert!(host.calls.lock().unwrap().is_empty());
}

#[test]
fn batches() {
    let host = FakeHost::default();
    let r = call(
        &host,
        json!([
            request(1, "ping", Value::Null),
            { "jsonrpc": "2.0", "method": "notifications/initialized" },
            request(2, "ping", Value::Null),
        ]),
    )
    .unwrap();
    assert_eq!(r.as_array().map(Vec::len), Some(2));
    assert_eq!(
        call(&host, json!([{ "jsonrpc": "2.0", "method": "x" }])),
        None
    );
    assert_eq!(
        call(&host, json!([])).unwrap()["error"]["code"],
        INVALID_REQUEST
    );
}
