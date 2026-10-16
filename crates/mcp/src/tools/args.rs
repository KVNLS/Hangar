use serde_json::{Map, Value};

pub(super) const TOOL_NAMES: [&str; 16] = [
    "device_info",
    "screenshot",
    "tap",
    "swipe",
    "type_text",
    "press",
    "logs",
    "build_and_run",
    "launch_app",
    "device_acquire",
    "device_release",
    "ask_user",
    "workspaces_list",
    "workspace_create",
    "agent_prompt",
    "agent_transcript",
];

pub(super) const ORCHESTRATION_TOOLS: [&str; 4] = [
    "workspaces_list",
    "workspace_create",
    "agent_prompt",
    "agent_transcript",
];

pub(super) fn allowed_args(tool: &str) -> &'static [&'static str] {
    match tool {
        "tap" => &["x", "y"],
        "swipe" => &["from_x", "from_y", "to_x", "to_y", "duration_ms"],
        "type_text" => &["text"],
        "press" => &["button", "key"],
        "logs" => &["lines", "level", "process"],
        "launch_app" => &["app_id"],
        "device_acquire" => &["wait_s"],
        "ask_user" => &["question", "choices"],
        "workspace_create" => &["task", "name", "agent"],
        "agent_prompt" => &["workspace", "text"],
        "agent_transcript" => &["workspace", "last"],
        _ => &[],
    }
}

pub(super) fn at_most(name: &str, value: &str, max: usize) -> Result<(), String> {
    if value.chars().count() > max {
        return Err(format!("{name:?} must have at most {max} characters"));
    }
    Ok(())
}

pub(super) fn kind(v: &Value) -> &'static str {
    match v {
        Value::Null => "null",
        Value::Bool(_) => "a boolean",
        Value::Number(_) => "a number",
        Value::String(_) => "a string",
        Value::Array(_) => "an array",
        Value::Object(_) => "an object",
    }
}

pub(super) struct Args<'a> {
    pub(super) args: &'a Map<String, Value>,
}

impl Args<'_> {
    pub(super) fn only(&self, allowed: &[&str]) -> Result<(), String> {
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
    pub(super) fn get(&self, name: &str) -> Option<&Value> {
        self.args.get(name).filter(|v| !v.is_null())
    }

    pub(super) fn point(&self, name: &str) -> Result<f64, String> {
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

    pub(super) fn integer(&self, name: &str, min: u64, max: u64) -> Result<Option<u64>, String> {
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

    pub(super) fn optional_string(&self, name: &str) -> Result<Option<String>, String> {
        match self.get(name) {
            None => Ok(None),
            Some(Value::String(s)) => Ok(Some(s.clone())),
            Some(v) => Err(format!("{name:?} must be a string, got {}", kind(v))),
        }
    }

    pub(super) fn required_string(&self, name: &str) -> Result<String, String> {
        self.optional_string(name)?
            .ok_or_else(|| format!("missing required string {name:?}"))
    }

    pub(super) fn workspace(&self) -> Result<String, String> {
        let workspace = self.required_string("workspace")?;
        if workspace.trim().is_empty() {
            return Err("\"workspace\" must not be empty".into());
        }
        Ok(workspace.trim().to_owned())
    }

    pub(super) fn choice<T: Copy>(
        &self,
        name: &str,
        options: &[(&str, T)],
    ) -> Result<Option<T>, String> {
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
