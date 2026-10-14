use serde_json::{Map, Value};

use super::args::{Args, TOOL_NAMES, allowed_args, at_most, kind};
use super::{
    BUTTONS, DEFAULT_LOG_LINES, DEFAULT_SWIPE_MS, DEFAULT_TRANSCRIPT_MESSAGES, KEYS, LEVELS,
    MAX_CHOICE_CHARS, MAX_CHOICES, MAX_LOG_LINES, MAX_NAME_CHARS, MAX_PROMPT_CHARS,
    MAX_QUESTION_CHARS, MAX_SWIPE_MS, MAX_TRANSCRIPT_MESSAGES, MAX_TYPE_TEXT_CHARS, ParseError,
    Press, Tool,
};

impl Tool {
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
