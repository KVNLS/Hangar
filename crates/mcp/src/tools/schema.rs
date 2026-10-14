use serde_json::{Value, json};

use super::{
    BUTTONS, DEFAULT_LOG_LINES, DEFAULT_SWIPE_MS, DEFAULT_TRANSCRIPT_MESSAGES, KEYS, LEVELS,
    MAX_CHOICE_CHARS, MAX_CHOICES, MAX_LOG_LINES, MAX_NAME_CHARS, MAX_PROMPT_CHARS,
    MAX_QUESTION_CHARS, MAX_SWIPE_MS, MAX_TRANSCRIPT_MESSAGES, MAX_TYPE_TEXT_CHARS,
};

fn names<T>(options: &[(&str, T)]) -> Vec<Value> {
    options.iter().map(|(n, _)| json!(n)).collect()
}

fn point_schema(description: &str) -> Value {
    json!({ "type": "number", "minimum": 0, "description": description })
}

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

pub(super) fn device_definitions() -> Vec<Value> {
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

pub(super) fn workspace_schema() -> Value {
    json!({
        "type": "string",
        "minLength": 1,
        "description": "The id (from workspaces_list or workspace_create) or exact name of a \
            workspace this agent created.",
    })
}

pub(super) fn orchestration_definitions() -> Vec<Value> {
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
