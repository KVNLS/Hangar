//! `xcrun simctl spawn <udid> log stream --style ndjson` streaming and parsing. Shared by the
//! private and simctl backends: the log stream needs only the booted simulator's UDID.

use std::process::Command;

use serde::Deserialize;

use crate::logs::{LogEntry, LogFilter, LogLevel, LogStream};
use crate::{Error, Result};

/// Streams the unified log of a booted simulator.
///
/// `filter.process` matches the process name, or (when it contains a dot) a subsystem prefix, so
/// both `MyApp` and `com.example.MyApp` work. `filter.history` is `Unsupported`: `log stream` has
/// no backlog.
pub fn log_stream(udid: &str, filter: &LogFilter) -> Result<LogStream> {
    if filter.history {
        return Err(Error::Unsupported("iOS log history"));
    }
    let mut cmd = Command::new("xcrun");
    cmd.args(["simctl", "spawn", udid, "log", "stream"]);
    cmd.args(["--style", "ndjson", "--level", "debug"]);
    if let Some(p) = filter.process.as_deref() {
        cmd.args(["--predicate", &predicate(p)?]);
    }
    LogStream::spawn(cmd, filter.min_level, filter.capacity, parse_line)
}

fn predicate(process: &str) -> Result<String> {
    if process.is_empty() || process.chars().any(char::is_control) {
        return Err(Error::Parse(format!("invalid process filter {process:?}")));
    }
    let q = process.replace('\\', "\\\\").replace('"', "\\\"");
    Ok(if process.contains('.') {
        format!("subsystem BEGINSWITH \"{q}\" OR process == \"{q}\"")
    } else {
        format!("process == \"{q}\"")
    })
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Raw {
    #[serde(default)]
    message_type: String,
    #[serde(default)]
    event_message: String,
    #[serde(default)]
    timestamp: String,
    #[serde(default, rename = "processID")]
    process_id: Option<u32>,
    #[serde(default, rename = "threadID")]
    thread_id: Option<u64>,
    #[serde(default)]
    process_image_path: String,
    #[serde(default)]
    subsystem: String,
    #[serde(default)]
    category: String,
}

fn level(message_type: &str) -> LogLevel {
    match message_type {
        "Debug" => LogLevel::Debug,
        "Error" => LogLevel::Error,
        "Fault" => LogLevel::Fault,
        _ => LogLevel::Info,
    }
}

/// One ndjson line. `None` for the banner/non-JSON lines and for non-log events (activity
/// creations, signposts without a message).
pub fn parse_line(line: &str) -> Option<LogEntry> {
    if !line.starts_with('{') {
        return None;
    }
    let raw: Raw = serde_json::from_str(line).ok()?;
    let process = raw
        .process_image_path
        .rsplit('/')
        .next()
        .unwrap_or_default()
        .to_owned();
    let mut e = LogEntry::new(level(&raw.message_type), raw.event_message);
    e.timestamp = raw.timestamp;
    e.pid = raw.process_id;
    e.tid = raw.thread_id.and_then(|t| u32::try_from(t).ok());
    e.tag = if raw.subsystem.is_empty() {
        process.clone()
    } else {
        raw.subsystem
    };
    e.category = raw.category;
    e.process = process;
    Some(e)
}

#[cfg(test)]
mod tests {
    use super::*;

    const FIXTURE: &str = r#"{"timezoneName":"","messageType":"Info","eventType":"logEvent","source":null,"formatString":"Request to block until connected","userID":501,"activityIdentifier":0,"subsystem":"com.apple.IDS","category":"DaemonController","threadID":1364622,"processImagePath":"\/Library\/Developer\/CoreSimulator\/Runtimes\/iOS.simruntime\/RuntimeRoot\/usr\/libexec\/rapportd","timestamp":"2026-10-04 20:44:53.605216+0200","eventMessage":"Request to block until connected","processID":4120}"#;

    #[test]
    fn parses_a_log_event() {
        let e = parse_line(FIXTURE).unwrap();
        assert_eq!(e.level, LogLevel::Info);
        assert_eq!(e.tag, "com.apple.IDS");
        assert_eq!(e.category, "DaemonController");
        assert_eq!(e.process, "rapportd");
        assert_eq!((e.pid, e.tid), (Some(4120), Some(1_364_622)));
        assert_eq!(e.timestamp, "2026-10-04 20:44:53.605216+0200");
        assert_eq!(e.message, "Request to block until connected");
    }

    #[test]
    fn maps_levels_and_falls_back_to_process_tag() {
        let line = |t: &str| {
            format!(
                r#"{{"messageType":"{t}","subsystem":"","processImagePath":"/bin/x","eventMessage":"a\nb \"q\""}}"#
            )
        };
        let levels: Vec<_> = ["Default", "Info", "Debug", "Error", "Fault"]
            .iter()
            .map(|t| parse_line(&line(t)).unwrap().level)
            .collect();
        assert_eq!(
            levels,
            [
                LogLevel::Info,
                LogLevel::Info,
                LogLevel::Debug,
                LogLevel::Error,
                LogLevel::Fault
            ]
        );
        let e = parse_line(&line("Default")).unwrap();
        assert_eq!(e.tag, "x");
        assert_eq!(e.message, "a\nb \"q\"");
    }

    #[test]
    fn skips_banners_and_garbage() {
        assert!(parse_line("Filtering the log data using \"process == x\"").is_none());
        assert!(parse_line("{not json").is_none());
        assert!(parse_line("").is_none());
    }

    #[test]
    fn builds_predicates() {
        assert_eq!(predicate("MyApp").unwrap(), r#"process == "MyApp""#);
        assert_eq!(
            predicate("com.ex.App").unwrap(),
            r#"subsystem BEGINSWITH "com.ex.App" OR process == "com.ex.App""#
        );
        assert_eq!(predicate(r#"a" OR 1"#).unwrap(), r#"process == "a\" OR 1""#);
        assert!(predicate("").is_err());
    }

    #[test]
    fn history_is_unsupported() {
        let f = LogFilter {
            history: true,
            ..LogFilter::default()
        };
        assert!(matches!(log_stream("udid", &f), Err(Error::Unsupported(_))));
    }
}
