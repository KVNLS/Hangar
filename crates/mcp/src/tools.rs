//! The device and orchestration tools: typed calls parsed from `tools/call`, their JSON Schemas,
//! and results.

use hangar_core::device::{Input, Key, LogLevel};

mod args;
mod output;
mod parse;
mod schema;

use args::ORCHESTRATION_TOOLS;

pub use output::{Answer, Content, DeviceStatus, ToolError, ToolOutput};
pub use schema::definitions;

pub const DEFAULT_SWIPE_MS: u64 = 300;
pub const MAX_SWIPE_MS: u64 = 10_000;
pub const DEFAULT_LOG_LINES: usize = 200;
pub const MAX_LOG_LINES: usize = 2000;
pub const DEFAULT_TRANSCRIPT_MESSAGES: usize = 10;
pub const MAX_TRANSCRIPT_MESSAGES: usize = 50;
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
}

#[cfg(test)]
mod tests;
