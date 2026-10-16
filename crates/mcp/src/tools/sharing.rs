use serde_json::{Value, json};

use super::output::ToolOutput;

/// A device's sharing state as seen by the calling workspace.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Sharing {
    pub device_name: String,
    pub device_id: Option<String>,
    /// Workspace holding the device; `None` when it is free.
    pub holder: Option<String>,
    pub you_hold: bool,
    /// 0 is next in line; `None` when the caller is not waiting.
    pub position: Option<usize>,
    /// Waiting workspaces, in order.
    pub queue: Vec<String>,
    /// Other workspaces bound to the same device.
    pub shared_with: Vec<String>,
    pub reservation_left_s: Option<u64>,
    /// An estimate, not a promise.
    pub estimated_wait_s: Option<u64>,
    pub idle_release_s: u64,
    /// The app on screen was installed by this other workspace.
    pub stale_app_from: Option<String>,
    /// A pending warning for the caller.
    pub notice: Option<String>,
}

/// "1st", "2nd", "11th" for a 1-based place.
pub fn ordinal(n: usize) -> String {
    let suffix = match (n % 100, n % 10) {
        (11..=13, _) => "th",
        (_, 1) => "st",
        (_, 2) => "nd",
        (_, 3) => "rd",
        _ => "th",
    };
    format!("{n}{suffix}")
}

fn duration(secs: u64) -> String {
    if secs < 60 {
        format!("about {secs} s")
    } else {
        format!("about {} min", secs.div_ceil(60))
    }
}

/// The error text a device tool returns when the device was not granted within its call budget.
pub fn queued_message(s: &Sharing) -> String {
    let mut out = s.device_name.clone();
    if !s.shared_with.is_empty() {
        out.push_str(&format!(" (shared with {})", s.shared_with.join(", ")));
    }
    match &s.holder {
        Some(holder) => out.push_str(&format!(" is in use by {holder}")),
        None => out.push_str(" is not available yet (starting up)"),
    }
    match s.position {
        Some(p) => out.push_str(&format!("; you are {} in line", ordinal(p + 1))),
        None => out.push_str("; you are waiting"),
    }
    match s.estimated_wait_s {
        Some(w) => out.push_str(&format!(", {}", duration(w))),
        None => out.push_str(", wait unknown"),
    }
    out.push_str(
        ". Your place is kept: call device_acquire with wait_s to wait, or device_info to check.",
    );
    out
}

impl ToolOutput {
    /// The `device_acquire` result (pretty JSON text).
    pub fn device_acquired(granted: bool, s: &Sharing) -> Self {
        let value = json!({
            "status": if granted { "granted" } else { "queued" },
            "device": { "name": s.device_name, "id": s.device_id },
            "shared_with": s.shared_with,
            "holder": s.holder,
            "position": s.position,
            "queue": s.queue,
            "estimated_wait_s": s.estimated_wait_s,
            "idle_release_s": s.idle_release_s,
            "reservation_left_s": s.reservation_left_s,
            "stale_app_from": s.stale_app_from,
            "notice": s.notice,
        });
        Self::text(serde_json::to_string_pretty(&value).unwrap_or_else(|_| value.to_string()))
    }

    /// The `device_release` result.
    pub fn device_released(had_waiters: bool, next: Option<&str>) -> Self {
        Self::text(match (had_waiters, next) {
            (_, Some(next)) => format!("Device released; {next} is next in line."),
            (true, None) => "Device released; the next workspace in line gets it.".to_owned(),
            (false, None) => "Device released; nobody was waiting.".to_owned(),
        })
    }

    pub(super) fn sharing_json(s: Option<&Sharing>) -> Value {
        let Some(s) = s else {
            return Value::Null;
        };
        json!({
            "holder": s.holder,
            "you_hold": s.you_hold,
            "position": s.position,
            "queue": s.queue,
            "shared_with": s.shared_with,
            "reservation_left_s": s.reservation_left_s,
            "estimated_wait_s": s.estimated_wait_s,
            "idle_release_s": s.idle_release_s,
            "stale_app_from": s.stale_app_from,
            "notice": s.notice,
        })
    }
}

#[cfg(test)]
mod tests;
