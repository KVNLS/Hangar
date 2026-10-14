//! The device tools: typed calls parsed from `tools/call`, their JSON Schemas, and results.

use hangar_core::device::{Input, Key, LogLevel, Orientation, Platform};
use serde_json::{Map, Value, json};

use crate::screenshot::Screenshot;

pub const DEFAULT_SWIPE_MS: u64 = 300;
pub const MAX_SWIPE_MS: u64 = 10_000;
pub const DEFAULT_LOG_LINES: usize = 200;
pub const MAX_LOG_LINES: usize = 2000;

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
        }
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
            _ => return Err(ParseError::UnknownTool(name.to_owned())),
        };
        Ok(parsed)
    }
}

const TOOL_NAMES: [&str; 9] = [
    "device_info",
    "screenshot",
    "tap",
    "swipe",
    "type_text",
    "press",
    "logs",
    "build_and_run",
    "launch_app",
];

fn allowed_args(tool: &str) -> &'static [&'static str] {
    match tool {
        "tap" => &["x", "y"],
        "swipe" => &["from_x", "from_y", "to_x", "to_y", "duration_ms"],
        "type_text" => &["text"],
        "press" => &["button", "key"],
        "logs" => &["lines", "level", "process"],
        "launch_app" => &["app_id"],
        _ => &[],
    }
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

/// The `tools` array of a `tools/list` result.
pub fn definitions() -> Vec<Value> {
    let no_args = json!({ "type": "object", "properties": {}, "additionalProperties": false });
    vec![
        json!({
            "name": "device_info",
            "description": "Describe the simulator/emulator attached to this Hangar workspace: platform, \
                name, id, screen size in points, orientation and whether it is booted.",
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
                    "text": { "type": "string", "minLength": 1, "description": "The text to type." },
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

impl ToolOutput {
    pub fn text(text: impl Into<String>) -> Self {
        Self {
            content: vec![Content::Text(text.into())],
        }
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
mod tests {
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
    fn every_definition_parses_its_name() {
        let defs = definitions();
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
}
