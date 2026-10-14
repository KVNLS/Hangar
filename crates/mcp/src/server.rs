//! App side: accepts bridge connections on a Unix socket and answers enveloped MCP messages.

use std::io::{self, BufRead, BufReader, Read, Write};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::Path;
use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::time::Duration;

use serde_json::Value;

use crate::protocol::{self, INVALID_REQUEST, PARSE_ERROR, ToolHost};

/// Longest message line a bridge may send; a longer one is refused and the connection closed.
pub const MAX_MESSAGE_BYTES: usize = 8 * 1024 * 1024;
/// Calls one connection may have in flight; the connection is not read further until one
/// answers.
pub const MAX_CALLS_PER_CONNECTION: usize = 16;
/// Calls in flight across every connection.
pub const MAX_CALLS: usize = 64;
/// How long an answer waits for a bridge that stopped reading before the connection is dropped.
const WRITE_TIMEOUT: Duration = Duration::from_secs(30);

static CALLS: Slots = Slots::new(MAX_CALLS);

/// Binds the app's socket at `path`, replacing a stale socket file left by a crashed run, and
/// restricts it to the current user (0600). Fails with `AddrInUse` if another process is
/// listening there. Put the socket in a directory only the user can enter: the mode is set right
/// after `bind`, so the directory closes that window. macOS limits the path to 103 bytes.
pub fn bind(path: &Path) -> io::Result<UnixListener> {
    match std::fs::symlink_metadata(path) {
        Ok(meta) => {
            use std::os::unix::fs::FileTypeExt;
            if !meta.file_type().is_socket() {
                return Err(io::Error::new(
                    io::ErrorKind::AlreadyExists,
                    format!("{} exists and is not a socket", path.display()),
                ));
            }
            if UnixStream::connect(path).is_ok() {
                return Err(io::Error::new(
                    io::ErrorKind::AddrInUse,
                    format!("another process is listening on {}", path.display()),
                ));
            }
            std::fs::remove_file(path)?;
        }
        Err(e) if e.kind() == io::ErrorKind::NotFound => {}
        Err(e) => return Err(e),
    }
    let listener = UnixListener::bind(path)?;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
    Ok(listener)
}

/// Accepts connections forever (blocking: run it on its own thread), one thread per connection
/// and one per message, so a long `build_and_run` never delays a `ping` or a screenshot. Calls
/// in flight are bounded per connection and overall.
pub fn serve(listener: UnixListener, host: Arc<dyn ToolHost>) {
    for stream in listener.incoming() {
        match stream {
            Ok(stream) => {
                let host = Arc::clone(&host);
                let spawned = std::thread::Builder::new()
                    .name("mcp-connection".into())
                    .spawn(move || serve_connection(stream, host.as_ref()));
                if let Err(e) = spawned {
                    tracing::warn!("mcp: cannot spawn connection thread: {e}");
                }
            }
            Err(e) => {
                tracing::warn!("mcp: accept failed: {e}");
                // Avoid spinning when out of file descriptors.
                std::thread::sleep(Duration::from_millis(100));
            }
        }
    }
}

/// Answers every line of one bridge connection; returns after the bridge closes its side and
/// all in-flight calls have answered.
pub fn serve_connection(stream: UnixStream, host: &dyn ToolHost) {
    if let Err(e) = stream.set_write_timeout(Some(WRITE_TIMEOUT)) {
        tracing::debug!("mcp: cannot set write timeout: {e}");
    }
    let writer = match stream.try_clone() {
        Ok(w) => Mutex::new(w),
        Err(e) => {
            tracing::warn!("mcp: cannot clone connection: {e}");
            return;
        }
    };
    let writer = &writer;
    let connection = Slots::new(MAX_CALLS_PER_CONNECTION);
    let connection = &connection;
    let mut reader = BufReader::new(stream);
    std::thread::scope(|scope| {
        loop {
            let line = match read_line(&mut reader) {
                Ok(Some(line)) => line,
                Ok(None) => break,
                Err(e) => {
                    tracing::debug!("mcp: connection read failed: {e}");
                    if e.kind() == io::ErrorKind::InvalidData {
                        let error = protocol::error_response(
                            &Value::Null,
                            INVALID_REQUEST,
                            &format!("message longer than {MAX_MESSAGE_BYTES} bytes"),
                        );
                        write_line(writer, error.to_string());
                    }
                    break;
                }
            };
            if line.trim().is_empty() {
                continue;
            }
            let slots = (connection.acquire(), CALLS.acquire());
            scope.spawn(move || {
                let _slots = slots;
                if let Some(response) = handle_envelope(host, &line) {
                    write_line(writer, response);
                }
            });
        }
    });
}

/// The next line without its newline, `None` at end of stream; `InvalidData` past
/// [`MAX_MESSAGE_BYTES`].
fn read_line(reader: &mut impl BufRead) -> io::Result<Option<String>> {
    let mut bytes = Vec::new();
    reader
        .take(MAX_MESSAGE_BYTES as u64 + 1)
        .read_until(b'\n', &mut bytes)?;
    if bytes.is_empty() {
        return Ok(None);
    }
    if bytes.last() == Some(&b'\n') {
        bytes.pop();
    } else if bytes.len() > MAX_MESSAGE_BYTES {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "message too long",
        ));
    }
    Ok(Some(String::from_utf8_lossy(&bytes).into_owned()))
}

fn write_line(writer: &Mutex<UnixStream>, line: String) {
    let mut out = match writer.lock() {
        Ok(out) => out,
        Err(poisoned) => poisoned.into_inner(),
    };
    let mut bytes = line.into_bytes();
    bytes.push(b'\n');
    if let Err(e) = out.write_all(&bytes) {
        tracing::debug!("mcp: connection write failed: {e}");
        // A bridge that stopped reading: drop it so its calls cannot pile up.
        let _ = out.shutdown(std::net::Shutdown::Both);
    }
}

/// A counting semaphore: [`Slots::acquire`] blocks while `max` slots are taken.
struct Slots {
    used: Mutex<usize>,
    freed: Condvar,
    max: usize,
}

struct Slot<'a>(&'a Slots);

impl Slots {
    const fn new(max: usize) -> Self {
        Self {
            used: Mutex::new(0),
            freed: Condvar::new(),
            max,
        }
    }

    fn lock(&self) -> MutexGuard<'_, usize> {
        self.used
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    fn acquire(&self) -> Slot<'_> {
        let mut used = self.lock();
        while *used >= self.max {
            used = self
                .freed
                .wait(used)
                .unwrap_or_else(|poisoned| poisoned.into_inner());
        }
        *used += 1;
        Slot(self)
    }
}

impl Drop for Slot<'_> {
    fn drop(&mut self) {
        *self.0.lock() -= 1;
        self.0.freed.notify_one();
    }
}

/// One envelope line `{"session": "...", "message": <JSON-RPC>}` to an optional response line.
pub fn handle_envelope(host: &dyn ToolHost, line: &str) -> Option<String> {
    let envelope: Value = match serde_json::from_str(line) {
        Ok(v) => v,
        Err(e) => {
            return Some(
                protocol::error_response(&Value::Null, PARSE_ERROR, &format!("parse error: {e}"))
                    .to_string(),
            );
        }
    };
    let session = envelope.get("session").and_then(Value::as_str);
    let message = envelope.get("message");
    match (session, message) {
        (Some(session), Some(message)) => {
            protocol::handle_value(host, session, message).map(|v| v.to_string())
        }
        _ => Some(
            protocol::error_response(
                &Value::Null,
                INVALID_REQUEST,
                "expected an envelope {\"session\": string, \"message\": JSON-RPC message}",
            )
            .to_string(),
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::tests::{FakeHost, SESSION};
    use serde_json::json;

    #[test]
    fn bind_sets_0600_and_replaces_stale_socket() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("mcp.sock");
        let listener = bind(&path).unwrap();
        let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);

        let err = bind(&path).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::AddrInUse);

        drop(listener);
        // The file outlives the listener: a crashed app leaves it behind.
        assert!(path.exists());
        let _listener = bind(&path).unwrap();
    }

    #[test]
    fn bind_refuses_to_delete_regular_files() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("not-a-socket");
        std::fs::write(&path, b"keep me").unwrap();
        assert!(bind(&path).is_err());
        assert_eq!(std::fs::read(&path).unwrap(), b"keep me");
    }

    #[test]
    fn oversized_message_is_refused_and_the_connection_closed() {
        let (app, mut bridge) = UnixStream::pair().unwrap();
        let server = std::thread::spawn(move || serve_connection(app, &FakeHost::default()));
        let writer = {
            let mut bridge = bridge.try_clone().unwrap();
            std::thread::spawn(move || {
                let chunk = vec![b'a'; 64 * 1024];
                for _ in 0..(MAX_MESSAGE_BYTES / chunk.len() + 2) {
                    if bridge.write_all(&chunk).is_err() {
                        break;
                    }
                }
            })
        };
        let mut reply = String::new();
        BufReader::new(&mut bridge).read_line(&mut reply).unwrap();
        let reply: Value = serde_json::from_str(&reply).unwrap();
        assert_eq!(reply["error"]["code"], INVALID_REQUEST);
        server.join().unwrap();
        writer.join().unwrap();
    }

    /// Blocks every call until released, recording how many ran at once.
    #[derive(Default)]
    struct SlowHost {
        state: Mutex<(usize, usize, bool)>,
        changed: Condvar,
    }

    impl ToolHost for SlowHost {
        fn knows_session(&self, _: &str) -> bool {
            true
        }

        fn call(&self, _: &str, tool: crate::Tool) -> Result<crate::ToolOutput, crate::ToolError> {
            let mut state = self.state.lock().unwrap();
            state.0 += 1;
            state.1 = state.1.max(state.0);
            self.changed.notify_all();
            while !state.2 {
                state = self.changed.wait(state).unwrap();
            }
            state.0 -= 1;
            Ok(crate::ToolOutput::text(tool.name().to_owned()))
        }

        fn orchestration_enabled(&self, _: &str) -> bool {
            false
        }

        fn agent_signal(
            &self,
            _: &str,
            _: hangar_core::AgentSignal,
            _: Option<&str>,
        ) -> Result<(), crate::ToolError> {
            Ok(())
        }
    }

    #[test]
    fn calls_in_flight_are_bounded_per_connection() {
        let host = Arc::new(SlowHost::default());
        let (app, mut bridge) = UnixStream::pair().unwrap();
        let served = Arc::clone(&host);
        let server = std::thread::spawn(move || serve_connection(app, served.as_ref()));
        for id in 0..MAX_CALLS_PER_CONNECTION * 3 {
            let call = json!({ "session": SESSION, "message": {
                "jsonrpc": "2.0", "id": id, "method": "tools/call",
                "params": { "name": "build_and_run", "arguments": {} } } });
            bridge.write_all(format!("{call}\n").as_bytes()).unwrap();
        }
        {
            let mut state = host.state.lock().unwrap();
            while state.0 < MAX_CALLS_PER_CONNECTION {
                state = host.changed.wait(state).unwrap();
            }
        }
        std::thread::sleep(Duration::from_millis(100));
        {
            let mut state = host.state.lock().unwrap();
            assert_eq!(state.1, MAX_CALLS_PER_CONNECTION);
            state.2 = true;
            host.changed.notify_all();
        }
        bridge.shutdown(std::net::Shutdown::Write).unwrap();
        let answers = BufReader::new(&mut bridge).lines().count();
        assert_eq!(answers, MAX_CALLS_PER_CONNECTION * 3);
        server.join().unwrap();
    }

    #[test]
    fn envelope_handling() {
        let host = FakeHost::default();
        let line = json!({ "session": SESSION, "message": { "jsonrpc": "2.0", "id": 1, "method": "ping" } });
        let r: Value =
            serde_json::from_str(&handle_envelope(&host, &line.to_string()).unwrap()).unwrap();
        assert_eq!(r["result"], json!({}));

        let line = json!({ "session": "nope", "message": { "jsonrpc": "2.0", "id": 1, "method": "ping" } });
        let r: Value =
            serde_json::from_str(&handle_envelope(&host, &line.to_string()).unwrap()).unwrap();
        assert_eq!(r["error"]["code"], protocol::UNKNOWN_SESSION);

        let line = json!({ "jsonrpc": "2.0", "id": 1, "method": "ping" });
        let r: Value =
            serde_json::from_str(&handle_envelope(&host, &line.to_string()).unwrap()).unwrap();
        assert_eq!(r["error"]["code"], INVALID_REQUEST);

        let r: Value = serde_json::from_str(&handle_envelope(&host, "]").unwrap()).unwrap();
        assert_eq!(r["error"]["code"], PARSE_ERROR);
    }
}
