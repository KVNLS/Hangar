//! The device and orchestration tools: typed calls parsed from `tools/call`, their JSON Schemas,
//! and results.

use hangar_core::device::{Input, Key, LogLevel, Orientation, Platform};
use serde_json::{Map, Value, json};

use crate::screenshot::Screenshot;

pub const DEFAULT_SWIPE_MS: u64 = 300;
pub const MAX_SWIPE_MS: u64 = 10_000;
pub const DEFAULT_LOG_LINES: usize = 200;
pub const MAX_LOG_LINES: usize = 2000;
pub const DEFAULT_TRANSCRIPT_MESSAGES: usize = 10;
pub const MAX_TRANSCRIPT_MESSAGES: usize = 50;

/// One validated tool call. Coordinates are device points, origin top-left of the display as
/// currently oriented (the space of [`hangar_core::device::Input`]).
#[derive(Debug, Clone, PartialEq)]
pub enum Tool {
    DeviceInfo,
    Screenshot,
    Tap {
        x: f64,
        y: f64,
    },
    Swipe {
        from_x: f64,
        from_y: f64,
        to_x: f64,
        to_y: f64,
        duration_ms: u64,
    },
    TypeText {
        text: String,
    },
    Press(Press),
    Logs {
        /// 1..=[`MAX_LOG_LINES`]: the most recent entries to return.
        lines: usize,
        min_level: Option<LogLevel>,
        process: Option<String>,
    },
    BuildAndRun,
    LaunchApp {
        app_id: String,
    },
    /// Asks the user a question and waits for the answer.
    AskUser {
        question: String,
        /// Answers to offer as buttons (the user may still type another one).
        choices: Vec<String>,
    },
    /// The workspaces the caller's agent created.
    WorkspacesList,
    WorkspaceCreate {
        task: String,
        name: Option<String>,
        /// Agent preset id; `None` runs the caller's own agent.
        agent: Option<String>,
    },
    AgentPrompt {
        /// A child workspace's id or name.
        workspace: String,
        text: String,
    },
    AgentTranscript {
        workspace: String,
        /// 1..=[`MAX_TRANSCRIPT_MESSAGES`]: the most recent messages to return.
        last: usize,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Press {
    Button(Button),
    Key(Key),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Button {
    Home,
    Back,
    Lock,
    VolumeUp,
    VolumeDown,
    AppSwitch,
}

impl Press {
    pub fn input(self) -> Input {
        match self {
            Self::Button(Button::Home) => Input::Home,
            Self::Button(Button::Back) => Input::Back,
            Self::Button(Button::Lock) => Input::Lock,
            Self::Button(Button::VolumeUp) => Input::VolumeUp,
            Self::Button(Button::VolumeDown) => Input::VolumeDown,
            Self::Button(Button::AppSwitch) => Input::AppSwitch,
            Self::Key(key) => Input::Key(key),
        }
    }
}

const BUTTONS: [(&str, Button); 6] = [
    ("home", Button::Home),
    ("back", Button::Back),
    ("lock", Button::Lock),
    ("volume_up", Button::VolumeUp),
    ("volume_down", Button::VolumeDown),
    ("app_switch", Button::AppSwitch),
];

const KEYS: [(&str, Key); 8] = [
    ("enter", Key::Enter),
    ("backspace", Key::Backspace),
    ("tab", Key::Tab),
    ("escape", Key::Escape),
    ("arrow_up", Key::ArrowUp),
    ("arrow_down", Key::ArrowDown),
    ("arrow_left", Key::ArrowLeft),
    ("arrow_right", Key::ArrowRight),
];

const LEVELS: [(&str, LogLevel); 5] = [
    ("verbose", LogLevel::Verbose),
    ("debug", LogLevel::Debug),
    ("info", LogLevel::Info),
    ("warning", LogLevel::Warn),
    ("error", LogLevel::Error),
];

/// Why a `tools/call` could not be turned into a [`Tool`].
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ParseError {
    /// Reported as a JSON-RPC `-32602` error, as the MCP spec does for unknown tools.
    #[error("unknown tool {0:?}")]
    UnknownTool(String),
    /// Reported as a tool result with `isError: true` so the model can correct itself.
    #[error("invalid arguments for {tool}: {message}")]
    InvalidArguments { tool: &'static str, message: String },
}

impl Tool {
    pub fn name(&self) -> &'static str {
        match self {
            Self::DeviceInfo => "device_info",
            Self::Screenshot => "screenshot",
            Self::Tap { .. } => "tap",
            Self::Swipe { .. } => "swipe",
            Self::TypeText { .. } => "type_text",
            Self::Press(_) => "press",
            Self::Logs { .. } => "logs",
            Self::BuildAndRun => "build_and_run",
            Self::LaunchApp { .. } => "launch_app",
            Self::AskUser { .. } => "ask_user",
            Self::WorkspacesList => "workspaces_list",
            Self::WorkspaceCreate { .. } => "workspace_create",
            Self::AgentPrompt { .. } => "agent_prompt",
            Self::AgentTranscript { .. } => "agent_transcript",
        }
    }

    /// Tools that create and drive other workspaces (off unless the user turned them on).
    pub fn is_orchestration_name(name: &str) -> bool {
        ORCHESTRATION_TOOLS.contains(&name)
    }

    /// Parses `tools/call` params. A missing or `null` `arguments` counts as `{}`.
    pub fn parse(name: &str, arguments: Option<&Value>) -> Result<Self, ParseError> {
        let empty = Map::new();
        let tool: &'static str = match TOOL_NAMES.iter().find(|n| **n == name) {
            Some(n) => n,
            None => return Err(ParseError::UnknownTool(name.to_owned())),
        };
        let invalid = |message: String| ParseError::InvalidArguments { tool, message };
        let args = match arguments {
            None | Some(Value::Null) => &empty,
            Some(Value::Object(map)) => map,
            Some(other) => {
                return Err(invalid(format!(
                    "arguments must be an object, got {}",
                    kind(other)
                )));
            }
        };
        let a = Args { args };
        a.only(allowed_args(tool)).map_err(invalid)?;
        let parsed = match tool {
            "device_info" => Self::DeviceInfo,
            "screenshot" => Self::Screenshot,
            "tap" => Self::Tap {
                x: a.point("x").map_err(invalid)?,
                y: a.point("y").map_err(invalid)?,
            },
            "swipe" => Self::Swipe {
                from_x: a.point("from_x").map_err(invalid)?,
                from_y: a.point("from_y").map_err(invalid)?,
                to_x: a.point("to_x").map_err(invalid)?,
                to_y: a.point("to_y").map_err(invalid)?,
                duration_ms: a
                    .integer("duration_ms", 0, MAX_SWIPE_MS)
                    .map_err(invalid)?
                    .unwrap_or(DEFAULT_SWIPE_MS),
            },
            "type_text" => {
                let text = a.required_string("text").map_err(invalid)?;
                if text.is_empty() {
                    return Err(invalid("\"text\" must not be empty".into()));
                }
                at_most("text", &text, MAX_TYPE_TEXT_CHARS).map_err(invalid)?;
                Self::TypeText { text }
            }
            "press" => {
                let button = a.choice("button", &BUTTONS).map_err(invalid)?;
                let key = a.choice("key", &KEYS).map_err(invalid)?;
                match (button, key) {
                    (Some(b), None) => Self::Press(Press::Button(b)),
                    (None, Some(k)) => Self::Press(Press::Key(k)),
                    (Some(_), Some(_)) => {
                        return Err(invalid(
                            "pass either \"button\" or \"key\", not both".into(),
                        ));
                    }
                    (None, None) => {
                        return Err(invalid("missing \"button\" or \"key\"".into()));
                    }
                }
            }
            "logs" => {
                let lines = a
                    .integer("lines", 1, MAX_LOG_LINES as u64)
                    .map_err(invalid)?
                    .map_or(DEFAULT_LOG_LINES, |n| n as usize);
                let process = a.optional_string("process").map_err(invalid)?;
                if process.as_deref() == Some("") {
                    return Err(invalid("\"process\" must not be empty".into()));
                }
                Self::Logs {
                    lines,
                    min_level: a.choice("level", &LEVELS).map_err(invalid)?,
                    process,
                }
            }
            "build_and_run" => Self::BuildAndRun,
            "launch_app" => {
                let app_id = a.required_string("app_id").map_err(invalid)?;
                if app_id.trim().is_empty() {
                    return Err(invalid("\"app_id\" must not be empty".into()));
                }
                Self::LaunchApp { app_id }
            }
            "workspaces_list" => Self::WorkspacesList,
            "workspace_create" => {
                let task = a.required_string("task").map_err(invalid)?;
                if task.trim().is_empty() {
                    return Err(invalid("\"task\" must not be empty".into()));
                }
                at_most("task", &task, MAX_PROMPT_CHARS).map_err(invalid)?;
                let name = a.optional_string("name").map_err(invalid)?;
                if let Some(name) = &name {
                    at_most("name", name, MAX_NAME_CHARS).map_err(invalid)?;
                }
                let agent = a.optional_string("agent").map_err(invalid)?;
                if agent.as_deref().is_some_and(|a| a.trim().is_empty()) {
                    return Err(invalid("\"agent\" must not be empty".into()));
                }
                Self::WorkspaceCreate {
                    task,
                    name: name.filter(|n| !n.trim().is_empty()),
                    agent: agent.map(|a| a.trim().to_owned()),
                }
            }
            "ask_user" => {
                let question = a.required_string("question").map_err(invalid)?;
                let question = question.trim().to_owned();
                if question.is_empty() || question.chars().count() > MAX_QUESTION_CHARS {
                    return Err(invalid(format!(
                        "\"question\" must have 1 to {MAX_QUESTION_CHARS} characters"
                    )));
                }
                let choices = match a.get("choices") {
                    None => Vec::new(),
                    Some(Value::Array(items)) => items
                        .iter()
                        .map(|item| match item.as_str().map(str::trim) {
                            Some(c) if !c.is_empty() && c.chars().count() <= MAX_CHOICE_CHARS => {
                                Ok(c.to_owned())
                            }
                            _ => Err(invalid(format!(
                                "each choice must be a string of 1 to {MAX_CHOICE_CHARS} characters"
                            ))),
                        })
                        .collect::<Result<_, _>>()?,
                    Some(v) => {
                        return Err(invalid(format!(
                            "\"choices\" must be an array of strings, got {}",
                            kind(v)
                        )));
                    }
                };
                if choices.len() > MAX_CHOICES {
                    return Err(invalid(format!("at most {MAX_CHOICES} choices")));
                }
                Self::AskUser { question, choices }
            }
            "agent_prompt" => {
                let workspace = a.workspace().map_err(invalid)?;
                let text = a.required_string("text").map_err(invalid)?;
                if text.trim().is_empty() {
                    return Err(invalid("\"text\" must not be empty".into()));
                }
                at_most("text", &text, MAX_PROMPT_CHARS).map_err(invalid)?;
                Self::AgentPrompt { workspace, text }
            }
            "agent_transcript" => Self::AgentTranscript {
                workspace: a.workspace().map_err(invalid)?,
                last: a
                    .integer("last", 1, MAX_TRANSCRIPT_MESSAGES as u64)
                    .map_err(invalid)?
                    .map_or(DEFAULT_TRANSCRIPT_MESSAGES, |n| n as usize),
            },
            _ => return Err(ParseError::UnknownTool(name.to_owned())),
        };
        Ok(parsed)
    }
}

const TOOL_NAMES: [&str; 14] = [
    "device_info",
    "screenshot",
    "tap",
    "swipe",
    "type_text",
    "press",
    "logs",
    "build_and_run",
    "launch_app",
    "ask_user",
    "workspaces_list",
    "workspace_create",
    "agent_prompt",
    "agent_transcript",
];

const ORCHESTRATION_TOOLS: [&str; 4] = [
    "workspaces_list",
    "workspace_create",
    "agent_prompt",
    "agent_transcript",
];

fn allowed_args(tool: &str) -> &'static [&'static str] {
    match tool {
        "tap" => &["x", "y"],
        "swipe" => &["from_x", "from_y", "to_x", "to_y", "duration_ms"],
        "type_text" => &["text"],
        "press" => &["button", "key"],
        "logs" => &["lines", "level", "process"],
        "launch_app" => &["app_id"],
        "ask_user" => &["question", "choices"],
        "workspace_create" => &["task", "name", "agent"],
        "agent_prompt" => &["workspace", "text"],
        "agent_transcript" => &["workspace", "last"],
        _ => &[],
    }
}

fn at_most(name: &str, value: &str, max: usize) -> Result<(), String> {
    if value.chars().count() > max {
        return Err(format!("{name:?} must have at most {max} characters"));
    }
    Ok(())
}

fn kind(v: &Value) -> &'static str {
    match v {
        Value::Null => "null",
        Value::Bool(_) => "a boolean",
        Value::Number(_) => "a number",
        Value::String(_) => "a string",
        Value::Array(_) => "an array",
        Value::Object(_) => "an object",
    }
}

struct Args<'a> {
    args: &'a Map<String, Value>,
}

impl Args<'_> {
    fn only(&self, allowed: &[&str]) -> Result<(), String> {
        let mut unknown: Vec<&str> = self
            .args
            .keys()
            .map(String::as_str)
            .filter(|k| !allowed.contains(k))
            .collect();
        if unknown.is_empty() {
            return Ok(());
        }
        unknown.sort_unstable();
        let expected = if allowed.is_empty() {
            "this tool takes no arguments".to_owned()
        } else {
            format!("expected only {}", allowed.join(", "))
        };
        Err(format!(
            "unknown argument(s) {}; {expected}",
            unknown.join(", ")
        ))
    }

    /// Present and not null.
    fn get(&self, name: &str) -> Option<&Value> {
        self.args.get(name).filter(|v| !v.is_null())
    }

    fn point(&self, name: &str) -> Result<f64, String> {
        let v = self
            .get(name)
            .ok_or_else(|| format!("missing required number {name:?}"))?;
        let n = v
            .as_f64()
            .ok_or_else(|| format!("{name:?} must be a number, got {}", kind(v)))?;
        if !n.is_finite() || n < 0.0 {
            return Err(format!(
                "{name:?} must be a non-negative number of points, got {n}"
            ));
        }
        Ok(n)
    }

    fn integer(&self, name: &str, min: u64, max: u64) -> Result<Option<u64>, String> {
        let Some(v) = self.get(name) else {
            return Ok(None);
        };
        let out_of_range =
            || format!("{name:?} must be an integer between {min} and {max}, got {v}");
        let n = match v.as_u64() {
            Some(n) => n,
            None => match v.as_f64() {
                // JSON clients may send integral values as floats (`300.0`).
                Some(f) if f.fract() == 0.0 && f >= 0.0 && f <= max as f64 => f as u64,
                Some(_) => return Err(out_of_range()),
                None => return Err(format!("{name:?} must be an integer, got {}", kind(v))),
            },
        };
        if !(min..=max).contains(&n) {
            return Err(out_of_range());
        }
        Ok(Some(n))
    }

    fn optional_string(&self, name: &str) -> Result<Option<String>, String> {
        match self.get(name) {
            None => Ok(None),
            Some(Value::String(s)) => Ok(Some(s.clone())),
            Some(v) => Err(format!("{name:?} must be a string, got {}", kind(v))),
        }
    }

    fn required_string(&self, name: &str) -> Result<String, String> {
        self.optional_string(name)?
            .ok_or_else(|| format!("missing required string {name:?}"))
    }

    fn workspace(&self) -> Result<String, String> {
        let workspace = self.required_string("workspace")?;
        if workspace.trim().is_empty() {
            return Err("\"workspace\" must not be empty".into());
        }
        Ok(workspace.trim().to_owned())
    }

    fn choice<T: Copy>(&self, name: &str, options: &[(&str, T)]) -> Result<Option<T>, String> {
        let Some(s) = self.optional_string(name)? else {
            return Ok(None);
        };
        match options.iter().find(|(n, _)| *n == s) {
            Some((_, value)) => Ok(Some(*value)),
            None => {
                let names: Vec<&str> = options.iter().map(|(n, _)| *n).collect();
                Err(format!(
                    "{name:?} must be one of {}, got {s:?}",
                    names.join(", ")
                ))
            }
        }
    }
}

fn names<T>(options: &[(&str, T)]) -> Vec<Value> {
    options.iter().map(|(n, _)| json!(n)).collect()
}

fn point_schema(description: &str) -> Value {
    json!({ "type": "number", "minimum": 0, "description": description })
}

/// Longest `type_text` text, in characters.
pub const MAX_TYPE_TEXT_CHARS: usize = 10_000;
/// Longest `agent_prompt` text or `workspace_create` task, in characters.
pub const MAX_PROMPT_CHARS: usize = 100_000;
/// Longest `workspace_create` name, in characters.
pub const MAX_NAME_CHARS: usize = 100;
/// Longest `ask_user` question, in characters.
pub const MAX_QUESTION_CHARS: usize = 2000;
/// Most `ask_user` choices, and the longest one.
pub const MAX_CHOICES: usize = 6;
pub const MAX_CHOICE_CHARS: usize = 80;

/// The `tools` array of a `tools/list` result: the device tools and `ask_user`, then the
/// orchestration tools when they are on.
pub fn definitions(orchestration: bool) -> Vec<Value> {
    let mut tools = device_definitions();
    tools.push(json!({
        "name": "ask_user",
        "description": "Ask the user a question and wait for the answer (it may take minutes). \
            Use it when a decision is theirs to make: an ambiguous requirement, a risky step, a \
            choice between approaches. Offer choices when the likely answers are known; the \
            user can still type their own. Returns {\"answer\", \"choice\"} (the chosen index, \
            or null for a typed answer).",
        "inputSchema": {
            "type": "object",
            "properties": {
                "question": {
                    "type": "string",
                    "maxLength": MAX_QUESTION_CHARS,
                    "description": "The question, self-contained: the user may not have followed your work.",
                },
                "choices": {
                    "type": "array",
                    "items": { "type": "string", "maxLength": MAX_CHOICE_CHARS },
                    "maxItems": MAX_CHOICES,
                    "description": "Suggested answers, shown as buttons.",
                },
            },
            "required": ["question"],
            "additionalProperties": false,
        },
    }));
    if orchestration {
        tools.extend(orchestration_definitions());
    }
    tools
}

fn device_definitions() -> Vec<Value> {
    let no_args = json!({ "type": "object", "properties": {}, "additionalProperties": false });
    vec![
        json!({
            "name": "device_info",
            "description": "Describe the simulator/emulator attached to this Hangar workspace: platform, \
                name, id, screen size in points, orientation and whether it is booted. Does not boot it. \
                While booted, id is the target for xcrun simctl (UDID) or adb -s (serial); while not, \
                it is not, and a screenshot boots the device.",
            "inputSchema": no_args,
        }),
        json!({
            "name": "screenshot",
            "description": "Capture the device screen as a PNG (longest side at most 1024 px). The text \
                part gives the screen size in points: tap/swipe coordinates are points, origin top-left, \
                in the current orientation, so scale image pixels by points/pixels.",
            "inputSchema": no_args,
        }),
        json!({
            "name": "tap",
            "description": "Tap the screen at a point (device points, origin top-left, current orientation).",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "x": point_schema("Horizontal position in points from the left edge."),
                    "y": point_schema("Vertical position in points from the top edge."),
                },
                "required": ["x", "y"],
                "additionalProperties": false,
            },
        }),
        json!({
            "name": "swipe",
            "description": "Drag one finger from one point to another (device points, origin top-left). \
                Use it to scroll: swipe up (from a larger y to a smaller y) to scroll content down.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "from_x": point_schema("Start x in points."),
                    "from_y": point_schema("Start y in points."),
                    "to_x": point_schema("End x in points."),
                    "to_y": point_schema("End y in points."),
                    "duration_ms": {
                        "type": "integer",
                        "minimum": 0,
                        "maximum": MAX_SWIPE_MS,
                        "default": DEFAULT_SWIPE_MS,
                        "description": "How long the drag takes, in milliseconds.",
                    },
                },
                "required": ["from_x", "from_y", "to_x", "to_y"],
                "additionalProperties": false,
            },
        }),
        json!({
            "name": "type_text",
            "description": "Type text into the focused field, as if from a hardware keyboard. Tap the \
                field first to focus it.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "text": {
                        "type": "string",
                        "minLength": 1,
                        "maxLength": MAX_TYPE_TEXT_CHARS,
                        "description": "The text to type.",
                    },
                },
                "required": ["text"],
                "additionalProperties": false,
            },
        }),
        json!({
            "name": "press",
            "description": "Press a hardware button or a non-text key. Pass exactly one of `button` or \
                `key`. `back` and `app_switch` exist on Android only.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "button": { "type": "string", "enum": names(&BUTTONS), "description": "Hardware button." },
                    "key": { "type": "string", "enum": names(&KEYS), "description": "Keyboard key." },
                },
                "oneOf": [ { "required": ["button"] }, { "required": ["key"] } ],
                "additionalProperties": false,
            },
        }),
        json!({
            "name": "logs",
            "description": "Recent device log lines (oldest first), optionally filtered by minimum level \
                and process.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "lines": {
                        "type": "integer",
                        "minimum": 1,
                        "maximum": MAX_LOG_LINES,
                        "default": DEFAULT_LOG_LINES,
                        "description": "How many of the most recent lines to return.",
                    },
                    "level": {
                        "type": "string",
                        "enum": names(&LEVELS),
                        "description": "Minimum severity to include.",
                    },
                    "process": {
                        "type": "string",
                        "minLength": 1,
                        "description": "Only lines from this process (iOS: process name or bundle id; \
                            Android: package name).",
                    },
                },
                "additionalProperties": false,
            },
        }),
        json!({
            "name": "build_and_run",
            "description": "Build this workspace's app, install it on the device and launch it. Returns \
                once the app is launched or the build failed, with a summary (and the failing output).",
            "inputSchema": no_args,
        }),
        json!({
            "name": "launch_app",
            "description": "Launch an installed app by its bundle id (iOS) or package name (Android).",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "app_id": {
                        "type": "string",
                        "minLength": 1,
                        "description": "Bundle id (iOS) or package name (Android).",
                    },
                },
                "required": ["app_id"],
                "additionalProperties": false,
            },
        }),
    ]
}

fn workspace_schema() -> Value {
    json!({
        "type": "string",
        "minLength": 1,
        "description": "The id (from workspaces_list or workspace_create) or exact name of a \
            workspace this agent created.",
    })
}

fn orchestration_definitions() -> Vec<Value> {
    let no_args = json!({ "type": "object", "properties": {}, "additionalProperties": false });
    vec![
        json!({
            "name": "workspaces_list",
            "description": "List the Hangar workspaces this agent created with workspace_create \
                (archived ones are left out): id, name, task, branch, agent, agent state, diff \
                against the base branch when known, and whether the agent waits in the queue.",
            "inputSchema": no_args,
        }),
        json!({
            "name": "workspace_create",
            "description": "Create a child Hangar workspace (its own git worktree and branch, from \
                this workspace's base branch, in the same project) and start an agent in it with \
                `task` as its first prompt. The agent starts now or waits in Hangar's queue when \
                the maximum number of agents is running. Follow it with workspaces_list, \
                agent_transcript and agent_prompt. The user sees every workspace created.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "task": {
                        "type": "string",
                        "minLength": 1,
                        "maxLength": MAX_PROMPT_CHARS,
                        "description": "What the new agent should do; self-contained, it does not \
                            see this conversation.",
                    },
                    "name": {
                        "type": "string",
                        "maxLength": MAX_NAME_CHARS,
                        "description": "Workspace name (default: the task's first line).",
                    },
                    "agent": {
                        "type": "string",
                        "description": "Hangar agent preset id, e.g. claude, claude-acp, codex-acp, \
                            opencode-acp (default: the agent of this workspace).",
                    },
                },
                "required": ["task"],
                "additionalProperties": false,
            },
        }),
        json!({
            "name": "agent_prompt",
            "description": "Send a message to the agent of a workspace this agent created, as if \
                the user typed it. Fails while a structured agent is still working on its \
                previous message, or when no agent runs there.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "workspace": workspace_schema(),
                    "text": {
                        "type": "string",
                        "minLength": 1,
                        "maxLength": MAX_PROMPT_CHARS,
                        "description": "The message.",
                    },
                },
                "required": ["workspace", "text"],
                "additionalProperties": false,
            },
        }),
        json!({
            "name": "agent_transcript",
            "description": "The last messages of the agent in a workspace this agent created: \
                prompts, replies, tool calls with their status, permission requests. For a \
                terminal agent other than Claude Code, its recent screen and scrollback text.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "workspace": workspace_schema(),
                    "last": {
                        "type": "integer",
                        "minimum": 1,
                        "maximum": MAX_TRANSCRIPT_MESSAGES,
                        "default": DEFAULT_TRANSCRIPT_MESSAGES,
                        "description": "How many of the most recent messages to return.",
                    },
                },
                "required": ["workspace"],
                "additionalProperties": false,
            },
        }),
    ]
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Content {
    Text(String),
    /// PNG-encoded bytes (base64-encoded on the wire).
    Png(Vec<u8>),
}

/// A successful tool result.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ToolOutput {
    pub content: Vec<Content>,
}

/// The user's answer to `ask_user`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Answer {
    pub text: String,
    /// Index of the offered choice the user picked; `None` for a typed answer.
    pub choice: Option<usize>,
}

impl ToolOutput {
    pub fn text(text: impl Into<String>) -> Self {
        Self {
            content: vec![Content::Text(text.into())],
        }
    }

    /// `{"answer": "...", "choice": 1 | null}`.
    pub fn answer(answer: &Answer) -> Self {
        Self::text(json!({ "answer": answer.text, "choice": answer.choice }).to_string())
    }

    /// The image plus a line mapping its pixels to the device's point space.
    pub fn screenshot(shot: Screenshot, width_pt: f64, height_pt: f64) -> Self {
        let caption = format!(
            "Screenshot {}x{} px. Device screen is {} x {} points (origin top-left, current \
             orientation); tap at image pixel (px, py) with x = px * {:.4}, y = py * {:.4}.",
            shot.width,
            shot.height,
            fmt_points(width_pt),
            fmt_points(height_pt),
            width_pt / f64::from(shot.width.max(1)),
            height_pt / f64::from(shot.height.max(1)),
        );
        Self {
            content: vec![Content::Png(shot.png), Content::Text(caption)],
        }
    }

    /// The canonical `device_info` result (pretty JSON text).
    pub fn device_status(status: &DeviceStatus) -> Self {
        let platform = match status.platform {
            Platform::Ios => "ios",
            Platform::Android => "android",
        };
        let orientation = status.orientation.map(|o| match o {
            Orientation::Portrait => "portrait",
            Orientation::PortraitUpsideDown => "portrait_upside_down",
            Orientation::LandscapeLeft => "landscape_left",
            Orientation::LandscapeRight => "landscape_right",
        });
        let screen = status
            .screen_pt
            .map(|(w, h)| json!({ "width_pt": w, "height_pt": h, "scale": status.scale }));
        let value = json!({
            "platform": platform,
            "name": status.name,
            "id": status.id,
            "booted": status.booted,
            "screen": screen,
            "orientation": orientation,
        });
        Self::text(serde_json::to_string_pretty(&value).unwrap_or_else(|_| value.to_string()))
    }
}

fn fmt_points(v: f64) -> String {
    if v.fract() == 0.0 {
        format!("{v:.0}")
    } else {
        format!("{v:.1}")
    }
}

/// Facts for `device_info`; `None` where the device cannot tell (e.g. not booted).
#[derive(Debug, Clone, PartialEq)]
pub struct DeviceStatus {
    pub platform: Platform,
    pub name: String,
    pub id: String,
    pub booted: bool,
    /// Width and height in points, as currently oriented.
    pub screen_pt: Option<(f64, f64)>,
    pub scale: Option<f64>,
    pub orientation: Option<Orientation>,
}

/// A failed tool call.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ToolError {
    /// The session does not belong to an open workspace (JSON-RPC error `-32001`).
    #[error("unknown workspace session")]
    UnknownSession,
    /// The tool cannot work on this device or platform (result with `isError: true`).
    #[error("unsupported: {0}")]
    Unsupported(String),
    /// The tool ran and failed, e.g. no device is booted (result with `isError: true`).
    #[error("{0}")]
    Failed(String),
}

#[cfg(test)]
mod tests;
