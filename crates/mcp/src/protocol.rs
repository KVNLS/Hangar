//! MCP over JSON-RPC 2.0: one incoming message in, at most one response out. Stateless, so any
//! number of messages may be handled concurrently.

use hangar_core::AgentSignal;
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
    then screenshot and logs to verify. When a decision belongs to the user, ask_user asks them and \
    waits for the answer. The device starts on demand: every device tool except device_info \
    boots it. Before running xcrun simctl or adb yourself, call device_info and target its id \
    (simctl UDID, adb -s serial) only when booted is true; otherwise take a screenshot first. \
    HANGAR_DEVICE_ID and ANDROID_SERIAL are set when the agent starts and may be missing or stale.";

const ORCHESTRATION_INSTRUCTIONS: &str = " The user also lets this agent split work across \
    parallel Hangar workspaces: workspace_create starts another agent on a self-contained task in \
    its own worktree, workspaces_list and agent_transcript follow those agents, agent_prompt \
    sends them a message. They act only on workspaces this agent created.";

/// Shown when an orchestration tool is called while the user has not allowed them.
const ORCHESTRATION_OFF: &str = "orchestration tools are off: the user can allow them in Hangar \
    Settings › Permissions › Let agents create and drive workspaces";

/// The app side of the bridge: resolves workspace sessions and runs tools on their devices.
///
/// Called from a background thread per message (never the UI thread); calls may block for as
/// long as the tool takes (a build can take minutes) and may run concurrently.
pub trait ToolHost: Send + Sync {
    /// Whether `session` belongs to a workspace that is currently open.
    fn knows_session(&self, session: &str) -> bool;

    /// Runs `tool` on the device of the workspace owning `session`, or, for an orchestration
    /// tool, on the workspaces it created.
    fn call(&self, session: &str, tool: Tool) -> Result<ToolOutput, ToolError>;

    /// Whether the agent of `session` may use the orchestration tools (a user setting, off by
    /// default). They are neither listed nor callable otherwise.
    fn orchestration_enabled(&self, session: &str) -> bool;

    /// Records what the agent of the workspace owning `session` reports through its hooks
    /// (`Hangar notify`, see [`crate::hook`]).
    /// `transcript`: the agent's own transcript file as the hook reported it (Claude Code), to
    /// be checked by the host before it is ever read.
    fn agent_signal(
        &self,
        session: &str,
        signal: AgentSignal,
        transcript: Option<&str>,
    ) -> Result<(), ToolError>;
}

/// Hangar's own method (not MCP): `{"signal": "working" | "approval" | "input" | "turn_ended"}`.
pub const AGENT_SIGNAL_METHOD: &str = "hangar/agentSignal";

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
        "initialize" => Ok(initialize(params, host.orchestration_enabled(session))),
        "ping" => Ok(json!({})),
        "tools/list" => Ok(json!({
            "tools": tools::definitions(host.orchestration_enabled(session)),
        })),
        "tools/call" => tools_call(host, session, params),
        AGENT_SIGNAL_METHOD => agent_signal(host, session, params),
        _ => Err((METHOD_NOT_FOUND, format!("method not found: {method}"))),
    };
    Some(match result {
        Ok(result) => json!({ "jsonrpc": "2.0", "id": id, "result": result }),
        Err((code, msg)) => error_response(id, code, &msg),
    })
}

fn agent_signal(
    host: &dyn ToolHost,
    session: &str,
    params: Option<&Value>,
) -> Result<Value, (i64, String)> {
    let signal = params
        .and_then(|p| p.get("signal"))
        .and_then(Value::as_str)
        .and_then(AgentSignal::parse)
        .ok_or((
            INVALID_PARAMS,
            "params.signal must be working, approval, input or turn_ended".to_owned(),
        ))?;
    // The hook event and who raised it, for diagnosing states (`Hangar notify` sends them).
    let field = |name: &str| params.and_then(|p| p.get(name)).and_then(Value::as_str);
    tracing::debug!(
        signal = signal.as_str(),
        event = field("event"),
        agent = field("agent"),
        "agent signal from hook"
    );
    host.agent_signal(session, signal, field("transcript"))
        .map(|()| json!({}))
        .map_err(|e| match e {
            ToolError::UnknownSession => (UNKNOWN_SESSION, "unknown workspace session".to_owned()),
            other => (UNAVAILABLE, other.to_string()),
        })
}

fn initialize(params: Option<&Value>, orchestration: bool) -> Value {
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
        "instructions": if orchestration {
            format!("{INSTRUCTIONS}{ORCHESTRATION_INSTRUCTIONS}")
        } else {
            INSTRUCTIONS.to_owned()
        },
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
    if Tool::is_orchestration_name(name) && !host.orchestration_enabled(session) {
        return Ok(error_result(&format!("{name}: {ORCHESTRATION_OFF}")));
    }
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
pub(crate) mod tests;
