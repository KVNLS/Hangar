//! The subset of the Agent Client Protocol (v1) Hangar speaks as a client: message builders and
//! parsers over `serde_json::Value`.

use serde_json::{Value, json};

pub const PROTOCOL_VERSION: u64 = 1;

pub const METHOD_NOT_FOUND: i64 = -32601;
pub const INVALID_PARAMS: i64 = -32602;
/// The agent needs the user to sign in first.
pub const AUTH_REQUIRED: i64 = -32000;

/// An MCP server the agent should start for the session (stdio transport).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct McpServer {
    pub name: String,
    pub command: String,
    pub args: Vec<String>,
    pub env: Vec<(String, String)>,
}

impl McpServer {
    fn to_json(&self) -> Value {
        let env: Vec<Value> = self
            .env
            .iter()
            .map(|(name, value)| json!({ "name": name, "value": value }))
            .collect();
        json!({ "name": self.name, "command": self.command, "args": self.args, "env": env })
    }
}

pub fn initialize_params() -> Value {
    // No fs or terminal capabilities: the agent reads, writes and runs commands itself, inside
    // the worktree it was started in.
    json!({
        "protocolVersion": PROTOCOL_VERSION,
        "clientCapabilities": {
            "fs": { "readTextFile": false, "writeTextFile": false },
            "terminal": false,
            // claude-agent-acp: also send subagents' text that was not streamed.
            "_meta": { "subagent-transcript": true },
        },
        "clientInfo": { "name": "hangar", "version": env!("CARGO_PKG_VERSION") },
    })
}

pub fn new_session_params(cwd: &str, mcp_servers: &[McpServer]) -> Value {
    let servers: Vec<Value> = mcp_servers.iter().map(McpServer::to_json).collect();
    json!({ "cwd": cwd, "mcpServers": servers })
}

/// `session/load` params: the session to resume, replayed as `session/update`s before the answer.
pub fn load_session_params(session_id: &str, cwd: &str, mcp_servers: &[McpServer]) -> Value {
    let servers: Vec<Value> = mcp_servers.iter().map(McpServer::to_json).collect();
    json!({ "sessionId": session_id, "cwd": cwd, "mcpServers": servers })
}

pub fn prompt_params(session_id: &str, text: &str) -> Value {
    json!({ "sessionId": session_id, "prompt": [{ "type": "text", "text": text }] })
}

pub fn set_mode_params(session_id: &str, mode_id: &str) -> Value {
    json!({ "sessionId": session_id, "modeId": mode_id })
}

pub fn set_config_option_params(session_id: &str, config_id: &str, value: &str) -> Value {
    json!({ "sessionId": session_id, "configId": config_id, "value": value })
}

pub fn cancel_params(session_id: &str) -> Value {
    json!({ "sessionId": session_id })
}

pub fn permission_result(option_id: Option<&str>) -> Value {
    match option_id {
        Some(id) => json!({ "outcome": { "outcome": "selected", "optionId": id } }),
        None => json!({ "outcome": { "outcome": "cancelled" } }),
    }
}

/// What `initialize` told us about the agent.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct AgentInfo {
    pub protocol_version: u64,
    pub load_session: bool,
    pub auth_methods: Vec<String>,
}

pub fn parse_initialize(result: &Value) -> AgentInfo {
    AgentInfo {
        protocol_version: result["protocolVersion"].as_u64().unwrap_or_default(),
        load_session: result["agentCapabilities"]["loadSession"]
            .as_bool()
            .unwrap_or(false),
        auth_methods: result["authMethods"]
            .as_array()
            .map(|methods| {
                methods
                    .iter()
                    .filter_map(|m| m["name"].as_str().or(m["id"].as_str()))
                    .map(str::to_owned)
                    .collect()
            })
            .unwrap_or_default(),
    }
}

/// A session mode the agent offers (for Claude Code: its permission modes).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SessionMode {
    pub id: String,
    pub name: String,
}

/// The modes of a `session/new` or `session/load` result: the current mode id and the
/// available modes. Agents that only send `configOptions` (OpenCode) expose them as the option
/// of category `mode`, which `session/set_mode` still switches.
pub fn parse_modes(result: &Value) -> (Option<String>, Vec<SessionMode>) {
    let modes = &result["modes"];
    if modes.is_object() {
        return (
            modes["currentModeId"].as_str().map(str::to_owned),
            parse_mode_list(&modes["availableModes"], "id"),
        );
    }
    match mode_option(&result["configOptions"]) {
        Some(option) => (
            option["currentValue"].as_str().map(str::to_owned),
            parse_mode_list(&option["options"], "value"),
        ),
        None => (None, Vec::new()),
    }
}

fn parse_mode_list(items: &Value, id_key: &str) -> Vec<SessionMode> {
    items
        .as_array()
        .map(|modes| {
            modes
                .iter()
                .filter_map(|m| {
                    let id = m[id_key].as_str()?.to_owned();
                    let name = m["name"].as_str().unwrap_or(&id).to_owned();
                    Some(SessionMode { id, name })
                })
                .collect()
        })
        .unwrap_or_default()
}

fn mode_option(options: &Value) -> Option<&Value> {
    options
        .as_array()?
        .iter()
        .find(|o| o["category"].as_str() == Some("mode"))
}

/// A session setting the agent lets the client pick from (model, effort, mode, …).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ConfigOption {
    pub id: String,
    pub name: String,
    /// `mode`, `model`, `thought_level`, … (`None` when the agent gives none).
    pub category: Option<String>,
    pub current: String,
    pub values: Vec<ConfigValue>,
}

impl ConfigOption {
    pub fn is_mode(&self) -> bool {
        self.category.as_deref() == Some("mode")
    }

    /// Display name of the current value.
    pub fn current_name(&self) -> &str {
        self.values
            .iter()
            .find(|v| v.value == self.current)
            .map_or(&self.current, |v| &v.name)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ConfigValue {
    pub value: String,
    pub name: String,
}

/// The `select` entries of a `configOptions` array; grouped values are flattened.
pub fn parse_config_options(options: &Value) -> Vec<ConfigOption> {
    let Some(all) = options.as_array() else {
        return Vec::new();
    };
    all.iter()
        .filter(|o| o["type"].as_str() == Some("select"))
        .filter_map(|o| {
            let id = o["id"].as_str()?.to_owned();
            Some(ConfigOption {
                name: o["name"].as_str().unwrap_or(&id).to_owned(),
                category: o["category"].as_str().map(str::to_owned),
                current: o["currentValue"].as_str()?.to_owned(),
                values: select_values(&o["options"]),
                id,
            })
        })
        .collect()
}

fn select_values(items: &Value) -> Vec<ConfigValue> {
    let mut out = Vec::new();
    for item in items.as_array().into_iter().flatten() {
        if item["options"].is_array() {
            out.extend(select_values(&item["options"]));
        } else if let Some(value) = item["value"].as_str() {
            out.push(ConfigValue {
                value: value.to_owned(),
                name: item["name"].as_str().unwrap_or(value).to_owned(),
            });
        }
    }
    out
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StopReason {
    EndTurn,
    MaxTokens,
    MaxTurnRequests,
    Refusal,
    Cancelled,
}

impl StopReason {
    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "end_turn" => Self::EndTurn,
            "max_tokens" => Self::MaxTokens,
            "max_turn_requests" => Self::MaxTurnRequests,
            "refusal" => Self::Refusal,
            "cancelled" => Self::Cancelled,
            _ => return None,
        })
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ToolStatus {
    #[default]
    Pending,
    InProgress,
    Completed,
    Failed,
}

impl ToolStatus {
    fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "pending" => Self::Pending,
            "in_progress" => Self::InProgress,
            "completed" => Self::Completed,
            "failed" => Self::Failed,
            _ => return None,
        })
    }
}

/// A content block rendered as text. Non-text blocks become a short bracketed description so
/// the transcript says something was there.
pub fn content_text(block: &Value) -> String {
    match block["type"].as_str() {
        Some("text") => block["text"].as_str().unwrap_or_default().to_owned(),
        Some("resource_link") => {
            let uri = block["uri"].as_str().unwrap_or_default();
            let name = block["name"].as_str().unwrap_or(uri);
            format!("[{name}]({uri})")
        }
        Some("resource") => match block["resource"]["text"].as_str() {
            Some(text) => text.to_owned(),
            None => format!(
                "[resource {}]",
                block["resource"]["uri"].as_str().unwrap_or_default()
            ),
        },
        Some(other) => format!("[{other}]"),
        None => String::new(),
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ToolContent {
    Text(String),
    Diff {
        path: String,
        old_text: Option<String>,
        new_text: String,
    },
    /// Output of a client-side terminal; we never create those, so only the id is known.
    Terminal(String),
}

fn parse_tool_content(items: &Value) -> Option<Vec<ToolContent>> {
    let items = items.as_array()?;
    Some(
        items
            .iter()
            .filter_map(|item| match item["type"].as_str()? {
                "content" => Some(ToolContent::Text(content_text(&item["content"]))),
                "diff" => Some(ToolContent::Diff {
                    path: item["path"].as_str()?.to_owned(),
                    old_text: item["oldText"].as_str().map(str::to_owned),
                    new_text: item["newText"].as_str().unwrap_or_default().to_owned(),
                }),
                "terminal" => Some(ToolContent::Terminal(
                    item["terminalId"].as_str()?.to_owned(),
                )),
                _ => None,
            })
            .collect(),
    )
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Location {
    pub path: String,
    pub line: Option<u32>,
}

fn parse_locations(items: &Value) -> Option<Vec<Location>> {
    let items = items.as_array()?;
    Some(
        items
            .iter()
            .filter_map(|item| {
                Some(Location {
                    path: item["path"].as_str()?.to_owned(),
                    line: item["line"].as_u64().and_then(|l| u32::try_from(l).ok()),
                })
            })
            .collect(),
    )
}

/// A tool call as last reported by the agent.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ToolCall {
    pub id: String,
    pub title: String,
    /// `read`, `edit`, `delete`, `move`, `search`, `execute`, `think`, `fetch`, `switch_mode`,
    /// `other`.
    pub kind: String,
    pub status: ToolStatus,
    pub content: Vec<ToolContent>,
    pub locations: Vec<Location>,
    /// The agent's own tool name (Claude Code: `_meta.claudeCode.toolName`, e.g. `Agent`).
    pub tool_name: Option<String>,
    /// The subagent launch call this call was made under (`_meta.claudeCode.parentToolUseId`).
    pub parent: Option<String>,
    /// For a subagent launch, its agent type (`rawInput.subagent_type`).
    pub subagent_type: Option<String>,
    /// For a subagent launch, the task it was given (`rawInput.prompt`).
    pub prompt: Option<String>,
}

/// The fields a `tool_call_update` (or permission request) changes; `None` keeps the old value.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ToolCallUpdate {
    pub id: String,
    pub title: Option<String>,
    pub kind: Option<String>,
    pub status: Option<ToolStatus>,
    pub content: Option<Vec<ToolContent>>,
    pub locations: Option<Vec<Location>>,
    pub tool_name: Option<String>,
    pub parent: Option<String>,
    pub subagent_type: Option<String>,
    pub prompt: Option<String>,
}

/// The subagent launch an update was made under (Claude Code: `_meta.claudeCode.parentToolUseId`).
fn parent_of(update: &Value) -> Option<String> {
    update["_meta"]["claudeCode"]["parentToolUseId"]
        .as_str()
        .map(str::to_owned)
}

impl ToolCallUpdate {
    fn parse(v: &Value) -> Option<Self> {
        Some(Self {
            id: v["toolCallId"].as_str()?.to_owned(),
            title: v["title"].as_str().map(str::to_owned),
            kind: v["kind"].as_str().map(str::to_owned),
            status: v["status"].as_str().and_then(ToolStatus::parse),
            content: parse_tool_content(&v["content"]),
            locations: parse_locations(&v["locations"]),
            tool_name: v["_meta"]["claudeCode"]["toolName"]
                .as_str()
                .map(str::to_owned),
            parent: parent_of(v),
            subagent_type: v["rawInput"]["subagent_type"].as_str().map(str::to_owned),
            prompt: v["rawInput"]["prompt"].as_str().map(str::to_owned),
        })
    }
}

impl ToolCall {
    pub fn apply(&mut self, update: &ToolCallUpdate) {
        if let Some(title) = &update.title {
            self.title.clone_from(title);
        }
        if let Some(kind) = &update.kind {
            self.kind.clone_from(kind);
        }
        if let Some(status) = update.status {
            self.status = status;
        }
        if let Some(content) = &update.content {
            self.content.clone_from(content);
        }
        if let Some(locations) = &update.locations {
            self.locations.clone_from(locations);
        }
        // Later updates may omit the metadata; it never changes once known.
        for (field, value) in [
            (&mut self.tool_name, &update.tool_name),
            (&mut self.parent, &update.parent),
            (&mut self.subagent_type, &update.subagent_type),
            (&mut self.prompt, &update.prompt),
        ] {
            if value.is_some() {
                field.clone_from(value);
            }
        }
    }

    /// Launches a subagent (Claude Code's `Agent`, formerly `Task`, tool).
    pub fn is_subagent(&self) -> bool {
        matches!(self.tool_name.as_deref(), Some("Agent" | "Task"))
    }

    /// A call first seen through an update (allowed by the protocol).
    pub fn from_update(update: &ToolCallUpdate) -> Self {
        let mut call = Self {
            id: update.id.clone(),
            kind: "other".to_owned(),
            ..Self::default()
        };
        call.apply(update);
        call
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PlanStatus {
    Pending,
    InProgress,
    Completed,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PlanEntry {
    pub content: String,
    /// `high`, `medium` or `low`.
    pub priority: String,
    pub status: PlanStatus,
}

fn parse_plan(entries: &Value) -> Vec<PlanEntry> {
    entries
        .as_array()
        .map(|entries| {
            entries
                .iter()
                .map(|e| PlanEntry {
                    content: e["content"].as_str().unwrap_or_default().to_owned(),
                    priority: e["priority"].as_str().unwrap_or("medium").to_owned(),
                    status: match e["status"].as_str() {
                        Some("in_progress") => PlanStatus::InProgress,
                        Some("completed") => PlanStatus::Completed,
                        _ => PlanStatus::Pending,
                    },
                })
                .collect()
        })
        .unwrap_or_default()
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AvailableCommand {
    pub name: String,
    pub description: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SessionUpdate {
    /// `parent`: the subagent launch it came from; `None` for the agent itself.
    AgentMessage {
        text: String,
        parent: Option<String>,
    },
    AgentThought {
        text: String,
        parent: Option<String>,
    },
    UserMessage(String),
    ToolCall(ToolCall),
    ToolCallUpdate(ToolCallUpdate),
    /// The complete plan; replaces the previous one.
    Plan(Vec<PlanEntry>),
    AvailableCommands(Vec<AvailableCommand>),
    CurrentMode(String),
    /// The complete list of config options; replaces the previous one.
    ConfigOptions(Vec<ConfigOption>),
    /// An update kind this client does not know.
    Other {
        kind: String,
    },
}

/// Parses the `update` object of a `session/update` notification.
pub fn parse_update(update: &Value) -> Option<SessionUpdate> {
    let kind = update["sessionUpdate"].as_str()?;
    Some(match kind {
        "agent_message_chunk" => SessionUpdate::AgentMessage {
            text: content_text(&update["content"]),
            parent: parent_of(update),
        },
        "agent_thought_chunk" => SessionUpdate::AgentThought {
            text: content_text(&update["content"]),
            parent: parent_of(update),
        },
        "user_message_chunk" => SessionUpdate::UserMessage(content_text(&update["content"])),
        "tool_call" => {
            let fields = ToolCallUpdate::parse(update)?;
            let mut call = ToolCall::from_update(&fields);
            if fields.title.is_none() {
                call.title = "Tool call".to_owned();
            }
            SessionUpdate::ToolCall(call)
        }
        "tool_call_update" => SessionUpdate::ToolCallUpdate(ToolCallUpdate::parse(update)?),
        "plan" => SessionUpdate::Plan(parse_plan(&update["entries"])),
        "available_commands_update" => SessionUpdate::AvailableCommands(
            update["availableCommands"]
                .as_array()
                .map(|cmds| {
                    cmds.iter()
                        .filter_map(|c| {
                            Some(AvailableCommand {
                                name: c["name"].as_str()?.to_owned(),
                                description: c["description"]
                                    .as_str()
                                    .unwrap_or_default()
                                    .to_owned(),
                            })
                        })
                        .collect()
                })
                .unwrap_or_default(),
        ),
        "current_mode_update" => {
            SessionUpdate::CurrentMode(update["currentModeId"].as_str()?.to_owned())
        }
        "config_option_update" => {
            SessionUpdate::ConfigOptions(parse_config_options(&update["configOptions"]))
        }
        other => SessionUpdate::Other {
            kind: other.to_owned(),
        },
    })
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PermissionKind {
    AllowOnce,
    AllowAlways,
    RejectOnce,
    RejectAlways,
}

impl PermissionKind {
    pub fn is_allow(self) -> bool {
        matches!(self, Self::AllowOnce | Self::AllowAlways)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PermissionOption {
    pub id: String,
    pub name: String,
    pub kind: PermissionKind,
}

/// Parses `session/request_permission` params into the tool call and its options.
pub fn parse_permission(params: &Value) -> Option<(ToolCallUpdate, Vec<PermissionOption>)> {
    let tool_call = ToolCallUpdate::parse(&params["toolCall"])?;
    let options = params["options"]
        .as_array()?
        .iter()
        .filter_map(|o| {
            Some(PermissionOption {
                id: o["optionId"].as_str()?.to_owned(),
                name: o["name"].as_str()?.to_owned(),
                kind: match o["kind"].as_str()? {
                    "allow_once" => PermissionKind::AllowOnce,
                    "allow_always" => PermissionKind::AllowAlways,
                    "reject_once" => PermissionKind::RejectOnce,
                    "reject_always" => PermissionKind::RejectAlways,
                    _ => return None,
                },
            })
        })
        .collect();
    Some((tool_call, options))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_chunks_and_non_text_blocks() {
        let u = json!({"sessionUpdate": "agent_message_chunk", "content": {"type": "text", "text": "hi"}});
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

        let u =
            json!({"sessionUpdate": "tool_call_update", "toolCallId": "c1", "status": "completed"});
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
        let u = json!({"sessionUpdate": "usage_update", "used": 3});
        assert_eq!(
            parse_update(&u),
            Some(SessionUpdate::Other {
                kind: "usage_update".into()
            })
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
}
