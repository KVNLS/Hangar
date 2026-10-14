//! Device log streaming: a child process (`adb logcat`, `simctl spawn ... log stream`) whose output
//! is parsed into [`LogEntry`] values on a reader thread and handed over through a bounded queue.
//!
//! Everything here blocks or polls; nothing is async. The UI side calls [`LogStream::drain`] from a
//! timer, background consumers use [`LogStream::recv_timeout`].

use std::collections::VecDeque;
use std::io::{BufRead, BufReader, Read};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

pub use hangar_core::device::LogLevel;

use crate::{Error, Result};

/// Default queue capacity of a [`LogStream`].
pub const DEFAULT_CAPACITY: usize = 10_000;
const STDERR_TAIL: usize = 4096;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LogEntry {
    /// As printed by the device. Android: `MM-DD HH:MM:SS.mmm` (device local time, no year);
    /// iOS: `YYYY-MM-DD HH:MM:SS.ffffff+ZZZZ`.
    pub timestamp: String,
    pub level: LogLevel,
    pub pid: Option<u32>,
    pub tid: Option<u32>,
    /// Android: the log tag. iOS: the subsystem (or the process name when there is none).
    pub tag: String,
    /// iOS: the logging category. Empty on Android.
    pub category: String,
    /// iOS: executable name from `processImagePath`. Empty on Android.
    pub process: String,
    pub message: String,
    /// When the reader thread parsed the line (for latency measurements).
    pub received_at: Instant,
}

impl LogEntry {
    pub fn new(level: LogLevel, message: impl Into<String>) -> Self {
        Self {
            timestamp: String::new(),
            level,
            pid: None,
            tid: None,
            tag: String::new(),
            category: String::new(),
            process: String::new(),
            message: message.into(),
            received_at: Instant::now(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LogFilter {
    /// Entries below this level are discarded.
    pub min_level: Option<LogLevel>,
    /// Android: package name (resolved to its running pid, so the app must be running when the
    /// stream starts). iOS: process name, or a bundle id / subsystem prefix.
    pub process: Option<String>,
    /// Start with recent entries already in the device's buffer. Android: the last
    /// [`crate::android::logcat::HISTORY_LINES`] lines. iOS: not available (`Unsupported`).
    pub history: bool,
    /// Queue capacity; when full the oldest entry is dropped and counted.
    pub capacity: usize,
}

impl Default for LogFilter {
    fn default() -> Self {
        Self {
            min_level: None,
            process: None,
            history: false,
            capacity: DEFAULT_CAPACITY,
        }
    }
}

#[derive(Default)]
struct State {
    queue: VecDeque<LogEntry>,
    closed: bool,
    error: Option<String>,
}

struct Shared {
    state: Mutex<State>,
    ready: Condvar,
    dropped: AtomicU64,
}

impl Shared {
    fn lock(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }
}

/// A running log capture. Dropping it kills the child process and joins the reader threads.
pub struct LogStream {
    shared: Arc<Shared>,
    child: Child,
    reader: Option<JoinHandle<()>>,
}

impl LogStream {
    /// Spawns `cmd` and feeds each stdout line through `parse`. `parse` may keep state and returns
    /// `None` for lines that are not entries (markers). Entries below `min_level` are discarded.
    pub(crate) fn spawn(
        mut cmd: Command,
        min_level: Option<LogLevel>,
        capacity: usize,
        mut parse: impl FnMut(&str) -> Option<LogEntry> + Send + 'static,
    ) -> Result<Self> {
        let program = cmd.get_program().to_string_lossy().into_owned();
        let mut child = cmd
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| Error::Command(format!("{program}: {e}")))?;
        let (Some(stdout), Some(stderr)) = (child.stdout.take(), child.stderr.take()) else {
            let _ = child.kill();
            let _ = child.wait();
            return Err(Error::Command(format!("{program}: no stdio pipes")));
        };
        let shared = Arc::new(Shared {
            state: Mutex::new(State::default()),
            ready: Condvar::new(),
            dropped: AtomicU64::new(0),
        });
        let capacity = capacity.max(1);

        let stderr_tail = Arc::new(Mutex::new(Vec::<u8>::new()));
        let stderr_thread = {
            let tail = stderr_tail.clone();
            std::thread::Builder::new()
                .name("device-logs-stderr".into())
                .spawn(move || collect_tail(stderr, &tail))
        };
        let thread_shared = shared.clone();
        let spawned = std::thread::Builder::new()
            .name("device-logs".into())
            .spawn(move || {
                read_lines(stdout, |line| {
                    let Some(entry) = parse(line) else { return };
                    if min_level.is_some_and(|min| entry.level < min) {
                        return;
                    }
                    let mut st = thread_shared.lock();
                    if st.queue.len() >= capacity {
                        st.queue.pop_front();
                        thread_shared.dropped.fetch_add(1, Ordering::Relaxed);
                    }
                    st.queue.push_back(entry);
                    drop(st);
                    thread_shared.ready.notify_one();
                });
                // stdout hit EOF: the child exited (or was killed), so stderr ends too.
                if let Ok(h) = stderr_thread {
                    let _ = h.join();
                }
                let tail = stderr_tail.lock().unwrap_or_else(|e| e.into_inner());
                let text = String::from_utf8_lossy(&tail).trim().to_owned();
                drop(tail);
                let mut st = thread_shared.lock();
                st.closed = true;
                st.error = (!text.is_empty()).then_some(text);
                drop(st);
                thread_shared.ready.notify_all();
            });
        let reader = match spawned {
            Ok(h) => h,
            Err(e) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(Error::Io(e));
            }
        };
        Ok(Self {
            shared,
            child,
            reader: Some(reader),
        })
    }

    /// Next queued entry, if any. Never blocks.
    pub fn try_recv(&self) -> Option<LogEntry> {
        self.shared.lock().queue.pop_front()
    }

    /// All queued entries, oldest first. Never blocks.
    pub fn drain(&self) -> Vec<LogEntry> {
        self.shared.lock().queue.drain(..).collect()
    }

    /// Waits up to `timeout` for an entry. `None` on timeout, or when the stream ended and the
    /// queue is empty (check [`LogStream::is_closed`]).
    pub fn recv_timeout(&self, timeout: Duration) -> Option<LogEntry> {
        let deadline = Instant::now() + timeout;
        let mut st = self.shared.lock();
        loop {
            if let Some(e) = st.queue.pop_front() {
                return Some(e);
            }
            let left = deadline.checked_duration_since(Instant::now())?;
            if st.closed {
                return None;
            }
            st = self
                .shared
                .ready
                .wait_timeout(st, left)
                .unwrap_or_else(|e| e.into_inner())
                .0;
        }
    }

    /// Entries discarded because the consumer was too slow (oldest first).
    pub fn dropped(&self) -> u64 {
        self.shared.dropped.load(Ordering::Relaxed)
    }

    /// The child process exited (device went away, adb server killed, ...). Entries may still be
    /// queued.
    pub fn is_closed(&self) -> bool {
        self.shared.lock().closed
    }

    /// Tail of the child's stderr once the stream has closed, if it printed anything.
    pub fn error(&self) -> Option<String> {
        self.shared.lock().error.clone()
    }
}

impl Drop for LogStream {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        if let Some(h) = self.reader.take() {
            let _ = h.join();
        }
    }
}

fn read_lines(source: impl Read, mut f: impl FnMut(&str)) {
    let mut reader = BufReader::new(source);
    let mut buf = Vec::new();
    loop {
        buf.clear();
        match reader.read_until(b'\n', &mut buf) {
            Ok(0) | Err(_) => return,
            Ok(_) => {
                while matches!(buf.last(), Some(b'\n' | b'\r')) {
                    buf.pop();
                }
                f(&String::from_utf8_lossy(&buf));
            }
        }
    }
}

fn collect_tail(mut source: impl Read, tail: &Mutex<Vec<u8>>) {
    let mut chunk = [0u8; 1024];
    while let Ok(n) = source.read(&mut chunk) {
        if n == 0 {
            return;
        }
        let mut t = tail.lock().unwrap_or_else(|e| e.into_inner());
        t.extend_from_slice(&chunk[..n]);
        let excess = t.len().saturating_sub(STDERR_TAIL);
        t.drain(..excess);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sh(script: &str) -> Command {
        let mut c = Command::new("sh");
        c.args(["-c", script]);
        c
    }

    fn parse(line: &str) -> Option<LogEntry> {
        let (lvl, msg) = line.split_once(' ')?;
        Some(LogEntry::new(lvl.parse().ok()?, msg))
    }

    #[test]
    fn level_order_and_parse() {
        assert!(LogLevel::Debug < LogLevel::Info && LogLevel::Error < LogLevel::Fault);
        assert_eq!("W".parse(), Ok(LogLevel::Warn));
        assert!("loud".parse::<LogLevel>().is_err());
    }

    #[test]
    fn streams_filters_and_reports_stderr() {
        let s = LogStream::spawn(
            sh("echo 'info a'; echo 'debug b'; echo 'error c'; echo oops >&2"),
            Some(LogLevel::Info),
            10,
            parse,
        )
        .unwrap();
        let mut got = Vec::new();
        while let Some(e) = s.recv_timeout(Duration::from_secs(5)) {
            got.push(e.message);
        }
        assert_eq!(got, ["a", "c"]);
        assert!(s.is_closed());
        assert_eq!(s.error().as_deref(), Some("oops"));
    }

    #[test]
    fn full_queue_drops_oldest() {
        let s = LogStream::spawn(
            sh("for i in 1 2 3 4 5; do echo \"info m$i\"; done"),
            None,
            2,
            parse,
        )
        .unwrap();
        while !s.is_closed() {
            std::thread::sleep(Duration::from_millis(10));
        }
        let msgs: Vec<_> = s.drain().into_iter().map(|e| e.message).collect();
        assert_eq!(msgs, ["m4", "m5"]);
        assert_eq!(s.dropped(), 3);
    }

    #[test]
    fn drop_kills_a_silent_child() {
        let s = LogStream::spawn(sh("exec sleep 60"), None, 2, parse).unwrap();
        let start = Instant::now();
        drop(s);
        assert!(start.elapsed() < Duration::from_secs(5));
    }

    #[test]
    fn missing_program_is_a_command_error() {
        let r = LogStream::spawn(Command::new("/nonexistent/hangar-logs"), None, 2, parse);
        assert!(matches!(r, Err(Error::Command(_))));
    }
}
