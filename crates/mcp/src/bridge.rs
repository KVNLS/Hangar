//! Agent side: `Hangar mcp-bridge` relays MCP stdio to the running app's socket.

use std::collections::HashMap;
use std::io::{self, BufRead, BufReader, Write};
use std::net::Shutdown;
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use serde_json::{Value, json};

use crate::protocol::{PARSE_ERROR, UNAVAILABLE, error_response};

/// [`run_bridge`] on the process's stdin/stdout, with the socket path and session secret read
/// from [`crate::SOCKET_ENV`] and [`crate::SESSION_ENV`]. Fails with `InvalidInput` naming the
/// variable when one is missing or empty (the agent was not started by Hangar).
pub fn run_bridge_from_env() -> io::Result<()> {
    let var = |name: &str| match std::env::var(name) {
        Ok(v) if !v.trim().is_empty() => Ok(v),
        _ => Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!(
                "{name} is not set: the Hangar MCP bridge only works inside an agent started \
                 from a Hangar workspace"
            ),
        )),
    };
    let socket = var(crate::SOCKET_ENV)?;
    let session = var(crate::SESSION_ENV)?;
    run_bridge(
        Path::new(&socket),
        session.trim(),
        io::stdin().lock(),
        io::stdout(),
    )
}

/// Relays newline-delimited JSON-RPC from `input` (the agent's stdin) to the app listening on
/// `socket`, wrapping each message in `{"session", "message"}`, and copies the app's responses to
/// `output` (the agent's stdout). Connects lazily and reconnects after the app went away; while
/// the app is unreachable every request is answered with a JSON-RPC error that says so.
///
/// Returns once `input` ends and the app has answered (or dropped) everything in flight.
/// Fails only when reading `input` or writing `output` fails.
pub fn run_bridge<R: BufRead, W: Write + Send>(
    socket: &Path,
    session: &str,
    input: R,
    output: W,
) -> io::Result<()> {
    let bridge = Bridge {
        socket,
        out: Mutex::new(output),
        state: Mutex::new(State::default()),
    };
    let bridge = &bridge;
    std::thread::scope(|scope| {
        for line in input.lines() {
            let line = line?;
            let line = line.trim();
            if line.is_empty() {
                continue;
            }
            let message: Value = match serde_json::from_str(line) {
                Ok(v) => v,
                Err(e) => {
                    let err =
                        error_response(&Value::Null, PARSE_ERROR, &format!("parse error: {e}"));
                    bridge.write_line(&err.to_string())?;
                    continue;
                }
            };
            let ids = request_ids(&message);
            let mut envelope = json!({ "session": session, "message": message }).to_string();
            envelope.push('\n');

            let mut state = lock(&bridge.state);
            if state.conn.is_none() {
                let connected = UnixStream::connect(socket)
                    .and_then(|stream| Ok((stream.try_clone()?, stream)));
                match connected {
                    Ok((reader, stream)) => {
                        state.generation += 1;
                        let generation = state.generation;
                        state.conn = Some((generation, Arc::new(stream)));
                        scope.spawn(move || bridge.read_responses(generation, reader));
                    }
                    Err(e) => {
                        drop(state);
                        bridge.fail(
                            &ids,
                            &format!(
                                "Hangar is not reachable ({}: {e}). Make sure the Hangar app is \
                                 running and this agent's workspace is open, then retry.",
                                socket.display()
                            ),
                        )?;
                        continue;
                    }
                }
            }
            let Some((generation, stream)) = state.conn.clone() else {
                continue;
            };
            for id in &ids {
                state
                    .pending
                    .insert(id.to_string(), (generation, id.clone()));
            }
            drop(state);

            if let Err(e) = (&*stream).write_all(envelope.as_bytes()) {
                let mut state = lock(&bridge.state);
                for id in &ids {
                    state.pending.remove(&id.to_string());
                }
                if state.conn.as_ref().is_some_and(|(g, _)| *g == generation) {
                    state.conn = None;
                }
                drop(state);
                let _ = stream.shutdown(Shutdown::Both);
                bridge.fail(&ids, &format!("lost the connection to Hangar: {e}"))?;
            }
        }
        if let Some((_, stream)) = lock(&bridge.state).conn.as_ref() {
            // Lets the app finish in-flight calls, then close, which ends the reader thread.
            let _ = stream.shutdown(Shutdown::Write);
        }
        Ok(())
    })
}

struct Bridge<'a, W> {
    socket: &'a Path,
    out: Mutex<W>,
    state: Mutex<State>,
}

#[derive(Default)]
struct State {
    generation: u64,
    conn: Option<(u64, Arc<UnixStream>)>,
    /// Requests sent and not yet answered, by serialized id: (connection generation, id).
    pending: HashMap<String, (u64, Value)>,
}

impl<W: Write> Bridge<'_, W> {
    fn write_line(&self, line: &str) -> io::Result<()> {
        let mut out = lock(&self.out);
        out.write_all(line.as_bytes())?;
        out.write_all(b"\n")?;
        out.flush()
    }

    fn fail(&self, ids: &[Value], message: &str) -> io::Result<()> {
        for id in ids {
            self.write_line(&error_response(id, UNAVAILABLE, message).to_string())?;
        }
        Ok(())
    }

    fn read_responses(&self, generation: u64, stream: UnixStream) {
        for line in BufReader::new(stream).lines() {
            let Ok(line) = line else { break };
            if line.trim().is_empty() {
                continue;
            }
            if let Ok(response) = serde_json::from_str::<Value>(&line) {
                let mut state = lock(&self.state);
                for id in response_ids(&response) {
                    state.pending.remove(&id.to_string());
                }
            }
            if self.write_line(&line).is_err() {
                break;
            }
        }
        let orphans: Vec<Value> = {
            let mut state = lock(&self.state);
            if state.conn.as_ref().is_some_and(|(g, _)| *g == generation) {
                state.conn = None;
            }
            let keys: Vec<String> = state
                .pending
                .iter()
                .filter(|(_, (g, _))| *g == generation)
                .map(|(k, _)| k.clone())
                .collect();
            keys.iter()
                .filter_map(|k| state.pending.remove(k))
                .map(|(_, id)| id)
                .collect()
        };
        let message = format!(
            "Hangar closed the connection ({}) before answering; it may have quit or closed this \
             workspace.",
            self.socket.display()
        );
        let _ = self.fail(&orphans, &message);
    }
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

fn each(message: &Value) -> impl Iterator<Item = &Value> {
    match message {
        Value::Array(items) => items.iter().collect::<Vec<_>>().into_iter(),
        other => vec![other].into_iter(),
    }
}

/// Ids of the requests in a client message (notifications and client responses have none).
fn request_ids(message: &Value) -> Vec<Value> {
    each(message)
        .filter(|m| m.get("method").is_some())
        .filter_map(|m| m.get("id").cloned())
        .collect()
}

fn response_ids(message: &Value) -> Vec<Value> {
    each(message)
        .filter(|m| m.get("result").is_some() || m.get("error").is_some())
        .filter_map(|m| m.get("id").cloned())
        .collect()
}

#[cfg(test)]
mod tests;
