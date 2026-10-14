//! MCP over JSON-RPC 2.0: one incoming message in, at most one response out. Stateless, so any
//! number of messages may be handled concurrently.

use serde_json::{Value, json};

use crate::base64;
use crate::tools::{self, Content, ParseError, Tool, ToolError, ToolOutput};

/// Newest first; the first entry is offered when the client asks for an unknown version.
pub const SUPPORTED_PROTOCOL_VERSIONS: [&str; 3] = ["2025-06-18", "2025-03-26", "2024-11-05"];

pub const PARSE_ERROR: i64 = -32700;
pub const INVALID_REQUEST: i64 = -32600;
pub const METHOD_NOT_FOUND: i64 = -32601;
pub const INVALID_PARAMS: i64 = -32602;
pub const UNKNOWN_SESSION: i64 = -32001;
/// Used by the bridge when the app cannot be reached.
pub const UNAVAILABLE: i64 = -32002;

const INSTRUCTIONS: &str = "These tools act on the iOS simulator or Android emulator attached to \
    the Hangar workspace this agent runs in. Take a screenshot to see the screen; tap and swipe \
    coordinates are device points with the origin at the top-left of the screen as currently \
    oriented (the screenshot result states the point size). Use build_and_run after code changes, \
    then screenshot and logs to verify.";

/// The app side of the bridge: resolves workspace sessions and runs tools on their devices.
///
/// Called from a background thread per message (never the UI thread); calls may block for as
/// long as the tool takes (a build can take minutes) and may run concurrently.
pub trait ToolHost: Send + Sync {
    /// Whether `session` belongs to a workspace that is currently open.
    fn knows_session(&self, session: &str) -> bool;

    /// Runs `tool` on the device of the workspace owning `session`.
    fn call(&self, session: &str, tool: Tool) -> Result<ToolOutput, ToolError>;
}

/// Handles one raw JSON-RPC message (or batch) from the client of workspace `session`.
/// Returns the serialized response, or `None` for notifications.
pub fn handle(host: &dyn ToolHost, session: &str, message: &str) -> Option<String> {
    match serde_json::from_str::<Value>(message) {
        Ok(value) => handle_value(host, session, &value).map(|v| v.to_string()),
        Err(e) => Some(
            error_response(&Value::Null, PARSE_ERROR, &format!("parse error: {e}")).to_string(),
        ),
    }
}

pub(crate) fn handle_value(host: &dyn ToolHost, session: &str, message: &Value) -> Option<Value> {
    match message {
        Value::Array(batch) if !batch.is_empty() => {
            let responses: Vec<Value> = batch
                .iter()
                .filter_map(|m| handle_single(host, session, m))
                .collect();
            (!responses.is_empty()).then_some(Value::Array(responses))
        }
        Value::Array(_) => Some(error_response(&Value::Null, INVALID_REQUEST, "empty batch")),
        _ => handle_single(host, session, message),
    }
}

fn handle_single(host: &dyn ToolHost, session: &str, message: &Value) -> Option<Value> {
    let Value::Object(obj) = message else {
        return Some(error_response(
            &Value::Null,
            INVALID_REQUEST,
            "message must be an object",
        ));
    };
    let id = obj.get("id");
    let Some(method) = obj.get("method").and_then(Value::as_str) else {
        // A response to a server request (we send none) or garbage: answer only if it looks
        // like a request.
        return match (id, obj.contains_key("method")) {
            (Some(id), true) => Some(error_response(
                id,
                INVALID_REQUEST,
                "method must be a string",
            )),
            (Some(_), false) if obj.contains_key("result") || obj.contains_key("error") => None,
            (Some(id), false) => Some(error_response(id, INVALID_REQUEST, "missing method")),
            (None, _) => None,
        };
    };
    let params = obj.get("params");
    let Some(id) = id else {
        // Notifications (initialized, cancelled, ...) need no action from a stateless server.
        return None;
    };
    if !host.knows_session(session) {
        return Some(unknown_session(id));
    }
    let result = match method {
        "initialize" => Ok(initialize(params)),
        "ping" => Ok(json!({})),
        "tools/list" => Ok(json!({ "tools": tools::definitions() })),
        "tools/call" => tools_call(host, session, params),
        _ => Err((METHOD_NOT_FOUND, format!("method not found: {method}"))),
    };
    Some(match result {
        Ok(result) => json!({ "jsonrpc": "2.0", "id": id, "result": result }),
        Err((code, msg)) => error_response(id, code, &msg),
    })
}

fn initialize(params: Option<&Value>) -> Value {
    let requested = params
        .and_then(|p| p.get("protocolVersion"))
        .and_then(Value::as_str);
    let version = requested
        .and_then(|r| SUPPORTED_PROTOCOL_VERSIONS.iter().find(|v| **v == r))
        .unwrap_or(&SUPPORTED_PROTOCOL_VERSIONS[0]);
    json!({
        "protocolVersion": version,
        "capabilities": { "tools": {} },
        "serverInfo": { "name": "hangar", "version": env!("CARGO_PKG_VERSION") },
        "instructions": INSTRUCTIONS,
    })
}

fn tools_call(
    host: &dyn ToolHost,
    session: &str,
    params: Option<&Value>,
) -> Result<Value, (i64, String)> {
    let Some(name) = params.and_then(|p| p.get("name")).and_then(Value::as_str) else {
        return Err((
            INVALID_PARAMS,
            "tools/call needs params.name (a string)".into(),
        ));
    };
    let arguments = params.and_then(|p| p.get("arguments"));
    let tool = match Tool::parse(name, arguments) {
        Ok(tool) => tool,
        Err(e @ ParseError::UnknownTool(_)) => return Err((INVALID_PARAMS, e.to_string())),
        Err(e @ ParseError::InvalidArguments { .. }) => return Ok(error_result(&e.to_string())),
    };
    let tool_name = tool.name();
    match host.call(session, tool) {
        Ok(output) => Ok(success_result(output)),
        Err(ToolError::UnknownSession) => {
            Err((UNKNOWN_SESSION, "unknown workspace session".into()))
        }
        Err(e) => Ok(error_result(&format!("{tool_name} failed: {e}"))),
    }
}

fn success_result(output: ToolOutput) -> Value {
    let content: Vec<Value> = output
        .content
        .into_iter()
        .map(|c| match c {
            Content::Text(text) => json!({ "type": "text", "text": text }),
            Content::Png(png) => json!({
                "type": "image",
                "data": base64::encode(&png),
                "mimeType": "image/png",
            }),
        })
        .collect();
    json!({ "content": content, "isError": false })
}

fn error_result(text: &str) -> Value {
    json!({ "content": [ { "type": "text", "text": text } ], "isError": true })
}

fn unknown_session(id: &Value) -> Value {
    error_response(
        id,
        UNKNOWN_SESSION,
        "unknown workspace session: this agent's Hangar workspace is closed or HANGAR_MCP_SESSION is stale",
    )
}

pub(crate) fn error_response(id: &Value, code: i64, message: &str) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "error": { "code": code, "message": message } })
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::tools::Press;
    use hangar_devices::Key;
    use std::sync::Mutex;

    pub const SESSION: &str = "tok-123";

    #[derive(Default)]
    pub struct FakeHost {
        pub calls: Mutex<Vec<(String, Tool)>>,
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
                Tool::Press(Press::Key(_)) => {
                    Err(ToolError::Unsupported("keys on this device".into()))
                }
                other => Ok(ToolOutput::text(format!("ok {}", other.name()))),
            }
        }
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
                "launch_app"
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
        let by_name =
            |n: &str| tools.iter().find(|t| t["name"] == n).unwrap()["inputSchema"].clone();
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
}
