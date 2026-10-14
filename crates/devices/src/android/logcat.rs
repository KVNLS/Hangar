//! `adb logcat -v threadtime` streaming and parsing.

use std::path::Path;
use std::process::Command;

use super::adb::Adb;
use super::sdk::AndroidSdk;
use crate::logs::{LogEntry, LogFilter, LogLevel, LogStream};
use crate::{Error, Result};

/// Lines of history requested (`logcat -T`) when [`LogFilter::history`] is set.
pub const HISTORY_LINES: u32 = 1000;

/// Streams the log of the device with adb serial `serial`, locating the SDK's adb.
pub fn logcat(serial: &str, filter: &LogFilter) -> Result<LogStream> {
    let sdk = AndroidSdk::locate()?;
    logcat_with(&sdk.adb, serial, filter)
}

/// Like [`logcat`] with an explicit adb binary.
pub fn logcat_with(adb: &Path, serial: &str, filter: &LogFilter) -> Result<LogStream> {
    let pid = match filter.process.as_deref() {
        Some(pkg) => Some(resolve_pid(&Adb::new(adb.to_path_buf()), serial, pkg)?),
        None => None,
    };
    let mut cmd = Command::new(adb);
    cmd.args(command_args(serial, filter, pid));
    let mut parser = Parser::default();
    LogStream::spawn(cmd, filter.min_level, filter.capacity, move |l| {
        parser.parse(l)
    })
}

fn resolve_pid(adb: &Adb, serial: &str, package: &str) -> Result<u32> {
    if package.is_empty()
        || !package
            .chars()
            .all(|c| c.is_alphanumeric() || "._-".contains(c))
    {
        return Err(Error::Parse(format!("invalid package name {package:?}")));
    }
    adb.shell(serial, &format!("pidof -s {package}"))
        .ok()
        .and_then(|out| out.trim().parse().ok())
        .ok_or_else(|| Error::NotFound(format!("{package} is not running on {serial}")))
}

/// Arguments after the adb binary.
fn command_args(serial: &str, filter: &LogFilter, pid: Option<u32>) -> Vec<String> {
    let mut args: Vec<String> = ["-s", serial, "logcat", "-v", "threadtime"]
        .map(str::to_owned)
        .into();
    // `-T 1` starts at the newest line instead of replaying the whole buffer.
    args.push("-T".into());
    args.push(if filter.history { HISTORY_LINES } else { 1 }.to_string());
    if let Some(pid) = pid {
        args.push(format!("--pid={pid}"));
    }
    if let Some(level) = filter.min_level {
        args.push(format!("*:{}", level_letter(level)));
    }
    args
}

fn level_letter(level: LogLevel) -> char {
    match level {
        LogLevel::Verbose => 'V',
        LogLevel::Debug => 'D',
        LogLevel::Info => 'I',
        LogLevel::Warn => 'W',
        LogLevel::Error => 'E',
        LogLevel::Fault => 'F',
    }
}

fn level_from_letter(c: char) -> Option<LogLevel> {
    Some(match c {
        'V' => LogLevel::Verbose,
        'D' => LogLevel::Debug,
        'I' => LogLevel::Info,
        'W' => LogLevel::Warn,
        'E' => LogLevel::Error,
        'F' | 'A' => LogLevel::Fault,
        _ => return None,
    })
}

/// Stateful line parser. `threadtime` prints a full header on every line of a multi-line message,
/// so those arrive as consecutive entries; a line without a header (stack traces from some
/// buffers) continues the previous entry's metadata.
#[derive(Default)]
pub struct Parser {
    last: Option<LogEntry>,
}

impl Parser {
    /// `None` for buffer markers (`--------- beginning of main`) and empty lines.
    pub fn parse(&mut self, line: &str) -> Option<LogEntry> {
        if line.is_empty() || line.starts_with("--------- ") {
            return None;
        }
        if let Some(entry) = parse_threadtime(line) {
            self.last = Some(entry.clone());
            return Some(entry);
        }
        let mut entry = self.last.clone()?;
        entry.message = line.to_owned();
        entry.received_at = std::time::Instant::now();
        Some(entry)
    }
}

/// `MM-DD HH:MM:SS.mmm  PID  TID L TAG: message`
pub fn parse_threadtime(line: &str) -> Option<LogEntry> {
    let b = line.as_bytes();
    // Cheap shape check before splitting: "MM-DD HH:MM:SS.mmm ".
    if b.len() < 19 || b[2] != b'-' || b[5] != b' ' || b[8] != b':' || b[11] != b':' {
        return None;
    }
    let (date, rest) = line.split_once(' ')?;
    let rest = rest.trim_start();
    let (time, rest) = rest.split_once(' ')?;
    let rest = rest.trim_start();
    let (pid, rest) = rest.split_once(' ')?;
    let rest = rest.trim_start();
    let (tid, rest) = rest.split_once(' ')?;
    let rest = rest.trim_start();
    let (lvl, rest) = rest.split_once(' ')?;
    let mut chars = lvl.chars();
    let level = level_from_letter(chars.next()?)?;
    if chars.next().is_some() {
        return None;
    }
    let (tag, message) = rest.split_once(": ").or_else(|| {
        // Empty message: "TAG:" at end of line.
        rest.strip_suffix(':').map(|t| (t, ""))
    })?;
    let mut entry = LogEntry::new(level, message);
    entry.timestamp = format!("{date} {time}");
    entry.pid = pid.parse().ok();
    entry.tid = tid.parse().ok();
    entry.tag = tag.trim_end().to_owned();
    Some(entry)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_threadtime_line() {
        let e = parse_threadtime(
            "10-04 20:43:12.345  1234  5678 I ActivityManager: Start proc com.example: for service",
        )
        .unwrap();
        assert_eq!(e.timestamp, "10-04 20:43:12.345");
        assert_eq!((e.pid, e.tid), (Some(1234), Some(5678)));
        assert_eq!(e.level, LogLevel::Info);
        assert_eq!(e.tag, "ActivityManager");
        assert_eq!(e.message, "Start proc com.example: for service");
    }

    #[test]
    fn parses_padded_tag_wide_pids_and_empty_message() {
        let e =
            parse_threadtime("01-02 03:04:05.006 12345 23456 E chatty  : uid=1000 expire 3 lines")
                .unwrap();
        assert_eq!(
            (e.pid, e.tid, e.level),
            (Some(12345), Some(23456), LogLevel::Error)
        );
        assert_eq!(e.tag, "chatty");
        let e = parse_threadtime("01-02 03:04:05.006   100   100 D Tag:").unwrap();
        assert_eq!((e.tag.as_str(), e.message.as_str()), ("Tag", ""));
    }

    #[test]
    fn rejects_non_entries() {
        assert!(parse_threadtime("--------- beginning of main").is_none());
        assert!(parse_threadtime("random text that is long enough").is_none());
        assert!(parse_threadtime("10-04 20:43:12.345  1 2 X Tag: m").is_none());
    }

    #[test]
    fn markers_skipped_and_continuations_inherit_metadata() {
        let mut p = Parser::default();
        assert!(p.parse("--------- beginning of main").is_none());
        assert!(p.parse("--------- beginning of system").is_none());
        assert!(p.parse("   stray before any header").is_none());
        let a = p
            .parse("10-04 20:43:12.345   10   11 E AndroidRuntime: FATAL EXCEPTION: main")
            .unwrap();
        let b = p
            .parse("10-04 20:43:12.345   10   11 E AndroidRuntime: java.lang.Boom")
            .unwrap();
        let c = p.parse("\tat com.example.Main.run(Main.java:42)").unwrap();
        assert_eq!(a.message, "FATAL EXCEPTION: main");
        assert_eq!(b.message, "java.lang.Boom");
        assert_eq!(c.message, "\tat com.example.Main.run(Main.java:42)");
        assert_eq!(
            (c.tag.as_str(), c.pid, c.level),
            ("AndroidRuntime", Some(10), LogLevel::Error)
        );
    }

    #[test]
    fn builds_args() {
        let f = LogFilter {
            min_level: Some(LogLevel::Warn),
            history: true,
            ..LogFilter::default()
        };
        assert_eq!(
            command_args("emulator-5554", &f, Some(42)),
            [
                "-s",
                "emulator-5554",
                "logcat",
                "-v",
                "threadtime",
                "-T",
                "1000",
                "--pid=42",
                "*:W"
            ]
        );
        let args = command_args("x", &LogFilter::default(), None);
        assert_eq!(args[args.len() - 2..], ["-T", "1"]);
    }

    #[test]
    fn rejects_hostile_package_names() {
        let adb = Adb::new("/nonexistent".into());
        assert!(matches!(
            resolve_pid(&adb, "s", "a; rm -rf /"),
            Err(Error::Parse(_))
        ));
    }
}
