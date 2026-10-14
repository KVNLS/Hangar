//! App side: accepts bridge connections on a Unix socket and answers enveloped MCP messages.

use std::io::{self, BufRead, BufReader, Write};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::Value;

use crate::protocol::{self, INVALID_REQUEST, PARSE_ERROR, ToolHost};

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
/// and one per message, so a long `build_and_run` never delays a `ping` or a screenshot.
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
    let writer = match stream.try_clone() {
        Ok(w) => Mutex::new(w),
        Err(e) => {
            tracing::warn!("mcp: cannot clone connection: {e}");
            return;
        }
    };
    let writer = &writer;
    std::thread::scope(|scope| {
        for line in BufReader::new(stream).lines() {
            let line = match line {
                Ok(line) => line,
                Err(e) => {
                    tracing::debug!("mcp: connection read failed: {e}");
                    break;
                }
            };
            if line.trim().is_empty() {
                continue;
            }
            scope.spawn(move || {
                let Some(response) = handle_envelope(host, &line) else {
                    return;
                };
                let mut out = match writer.lock() {
                    Ok(out) => out,
                    Err(poisoned) => poisoned.into_inner(),
                };
                let mut bytes = response.into_bytes();
                bytes.push(b'\n');
                if let Err(e) = out.write_all(&bytes) {
                    tracing::debug!("mcp: connection write failed: {e}");
                }
            });
        }
    });
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
