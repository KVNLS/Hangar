//! Agent side of `Hangar notify <event>`: a Claude Code hook command. It reads the hook's JSON
//! payload, maps the event to an [`AgentSignal`] and sends it to the app over the MCP socket as
//! `hangar/agentSignal`, authenticated like the bridge. It never fails the hook: Claude Code
//! must not be slowed or blocked by Hangar, so every problem is only reported on stderr.

use std::io::{self, BufRead, BufReader, Read, Write};
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::time::Duration;

use hangar_core::AgentSignal;
use serde_json::{Value, json};

use crate::protocol::AGENT_SIGNAL_METHOD;

/// Longest a hook waits for the app: the hook runs inside Claude Code's turn.
const HOOK_TIMEOUT: Duration = Duration::from_secs(2);

/// Claude Code hook events Hangar subscribes to (`Hangar notify <event>`).
pub const HOOK_EVENTS: [&str; 6] = [
    "UserPromptSubmit",
    "PreToolUse",
    "PostToolUse",
    "PermissionRequest",
    "Notification",
    "Stop",
];

/// The signal a hook `event` with `payload` (the hook's stdin) stands for, if any.
pub fn signal_for_hook(event: &str, payload: &Value) -> Option<AgentSignal> {
    match event {
        "UserPromptSubmit" | "PreToolUse" | "PostToolUse" => Some(AgentSignal::Working),
        "PermissionRequest" => Some(AgentSignal::Approval),
        "Stop" => Some(AgentSignal::TurnEnded),
        "Notification" => {
            let kind = payload.get("notification_type").and_then(Value::as_str);
            let message = payload
                .get("message")
                .and_then(Value::as_str)
                .unwrap_or_default();
            match kind {
                Some("permission_prompt") => Some(AgentSignal::Approval),
                Some("elicitation_dialog") => Some(AgentSignal::Input),
                // "idle_prompt" comes long after Stop; others (auth) are not states.
                Some(_) => None,
                None if message.to_lowercase().contains("permission") => {
                    Some(AgentSignal::Approval)
                }
                None => None,
            }
        }
        _ => None,
    }
}

/// Runs the hook: reads `input` (the payload), sends the signal for `event` to the app named by
/// [`crate::SOCKET_ENV`] with [`crate::SESSION_ENV`]. Errors are for the caller to print; the
/// process should still exit 0.
pub fn notify_from_env(event: &str, input: impl Read) -> io::Result<()> {
    let mut text = String::new();
    input.take(1 << 20).read_to_string(&mut text)?;
    let payload: Value = serde_json::from_str(&text).unwrap_or(Value::Null);
    let Some(signal) = signal_for_hook(event, &payload) else {
        return Ok(());
    };
    let var = |name: &str| {
        std::env::var(name)
            .ok()
            .filter(|v| !v.trim().is_empty())
            .ok_or_else(|| {
                io::Error::new(io::ErrorKind::InvalidInput, format!("{name} is not set"))
            })
    };
    let socket = var(crate::SOCKET_ENV)?;
    let session = var(crate::SESSION_ENV)?;
    let agent = payload
        .get("agent_type")
        .or_else(|| payload.get("agent_id"))
        .and_then(Value::as_str);
    // Claude Code's session transcript, so a lead agent can read this one (`agent_transcript`).
    let transcript = payload.get("transcript_path").and_then(Value::as_str);
    let extra = json!({ "event": event, "agent": agent, "transcript": transcript });
    send(Path::new(&socket), session.trim(), signal, extra)
}

/// Sends one `hangar/agentSignal` and waits (briefly) for the app's answer. `extra`: more
/// `params` fields (event, agent, transcript), all optional.
fn send(socket: &Path, session: &str, signal: AgentSignal, extra: Value) -> io::Result<()> {
    let mut params = json!({ "signal": signal.as_str() });
    if let (Some(params), Value::Object(extra)) = (params.as_object_mut(), extra) {
        params.extend(extra.into_iter().filter(|(_, v)| !v.is_null()));
    }
    let mut stream = UnixStream::connect(socket)?;
    stream.set_read_timeout(Some(HOOK_TIMEOUT))?;
    stream.set_write_timeout(Some(HOOK_TIMEOUT))?;
    let envelope = json!({
        "session": session,
        "message": {
            "jsonrpc": "2.0",
            "id": 1,
            "method": AGENT_SIGNAL_METHOD,
            "params": params,
        },
    });
    writeln!(stream, "{envelope}")?;
    let mut line = String::new();
    BufReader::new(&stream).read_line(&mut line)?;
    let answer: Value = serde_json::from_str(&line).map_err(io::Error::other)?;
    match answer.get("error") {
        None => Ok(()),
        Some(error) => Err(io::Error::other(format!(
            "Hangar refused the signal: {error}"
        ))),
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;
    use crate::protocol::tests::{FakeHost, SESSION};

    #[test]
    fn hook_events_map_to_signals() {
        let none = Value::Null;
        assert_eq!(
            signal_for_hook("UserPromptSubmit", &none),
            Some(AgentSignal::Working)
        );
        assert_eq!(
            signal_for_hook("PostToolUse", &none),
            Some(AgentSignal::Working)
        );
        assert_eq!(signal_for_hook("Stop", &none), Some(AgentSignal::TurnEnded));
        let note = |kind: &str| json!({ "notification_type": kind, "message": "…" });
        assert_eq!(
            signal_for_hook("Notification", &note("permission_prompt")),
            Some(AgentSignal::Approval)
        );
        assert_eq!(
            signal_for_hook("Notification", &note("elicitation_dialog")),
            Some(AgentSignal::Input)
        );
        assert_eq!(signal_for_hook("Notification", &note("idle_prompt")), None);
        let old = json!({ "message": "Claude needs your permission to use Bash" });
        assert_eq!(
            signal_for_hook("Notification", &old),
            Some(AgentSignal::Approval)
        );
        assert_eq!(signal_for_hook("SessionEnd", &none), None);
        assert!(
            HOOK_EVENTS
                .iter()
                .all(|e| *e == "Notification" || signal_for_hook(e, &none).is_some())
        );
    }

    #[test]
    fn a_signal_travels_over_the_socket() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("hangar.sock");
        let listener = crate::bind(&path).unwrap();
        let host = Arc::new(FakeHost::default());
        let served = Arc::clone(&host);
        std::thread::spawn(move || {
            for _ in 0..2 {
                let (stream, _) = listener.accept().unwrap();
                crate::serve_connection(stream, served.as_ref());
            }
        });
        send(&path, SESSION, AgentSignal::Approval, json!({})).unwrap();
        let stale = send(&path, "stale", AgentSignal::Working, json!({})).unwrap_err();
        assert!(stale.to_string().contains("-32001"), "{stale}");
        assert_eq!(*host.signals.lock().unwrap(), [AgentSignal::Approval]);
    }
}
