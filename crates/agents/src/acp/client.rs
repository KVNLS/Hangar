//! JSON-RPC over newline-delimited stdio: requests with matched responses, agent requests and
//! notifications turned into [`AcpEvent`]s.

use std::collections::HashMap;
use std::io::{BufRead, BufReader, Read, Write};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use serde_json::{Value, json};

use super::protocol::{
    self, METHOD_NOT_FOUND, PermissionOption, SessionUpdate, StopReason, ToolCallUpdate,
};
use crate::UpdateHook;
use crate::error::{Error, Result};

/// The id of a request the agent sent us, echoed back in our response.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RequestId(Value);

impl RequestId {
    fn key(&self) -> String {
        self.0.to_string()
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PermissionRequest {
    pub request_id: RequestId,
    pub tool_call: ToolCallUpdate,
    pub options: Vec<PermissionOption>,
}

#[derive(Clone, Debug, PartialEq)]
pub enum AcpEvent {
    Update(SessionUpdate),
    /// The agent waits for an answer through [`AcpConnection::answer_permission`].
    PermissionRequest(PermissionRequest),
    /// The prompt turn finished.
    TurnEnded(StopReason),
    /// The prompt request failed; the turn is over.
    TurnFailed(String),
    /// A message that could not be understood. The connection stays up.
    ProtocolError(String),
    /// The agent closed its stdout. Always the last event.
    Exited {
        code: Option<i32>,
        stderr_tail: String,
    },
}

enum Pending {
    Reply(Sender<std::result::Result<Value, RpcError>>),
    Prompt,
}

#[derive(Debug)]
struct RpcError {
    code: i64,
    message: String,
}

struct Shared<W> {
    /// `None` once the client closed the agent's stdin.
    writer: Mutex<Option<W>>,
    pending: Mutex<HashMap<u64, Pending>>,
    permissions: Mutex<HashMap<String, RequestId>>,
    events: Sender<AcpEvent>,
    hook: Option<UpdateHook>,
    next_id: AtomicU64,
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

impl<W: Write> Shared<W> {
    fn emit(&self, event: AcpEvent) {
        if self.events.send(event).is_ok()
            && let Some(hook) = &self.hook
        {
            hook();
        }
    }

    fn write(&self, message: &Value) -> std::io::Result<()> {
        let mut line = message.to_string();
        line.push('\n');
        let mut writer = lock(&self.writer);
        let writer = writer.as_mut().ok_or_else(|| {
            std::io::Error::new(std::io::ErrorKind::BrokenPipe, "agent stdin is closed")
        })?;
        writer.write_all(line.as_bytes())?;
        writer.flush()
    }
}

/// Produces the final [`AcpEvent::Exited`] once the agent's stdout has closed.
pub type OnClose = Box<dyn FnOnce() -> AcpEvent + Send>;

/// A client connection to one agent process.
pub struct AcpConnection<W: Write + Send + 'static> {
    shared: Arc<Shared<W>>,
}

impl<W: Write + Send + 'static> AcpConnection<W> {
    /// Starts the reader thread on `reader` (the agent's stdout). Events arrive on the returned
    /// receiver; `hook` runs after each one.
    pub fn start<R: Read + Send + 'static>(
        reader: R,
        writer: W,
        hook: Option<UpdateHook>,
        on_close: OnClose,
    ) -> Result<(Self, Receiver<AcpEvent>)> {
        let (events, rx) = mpsc::channel();
        let shared = Arc::new(Shared {
            writer: Mutex::new(Some(writer)),
            pending: Mutex::new(HashMap::new()),
            permissions: Mutex::new(HashMap::new()),
            events,
            hook,
            next_id: AtomicU64::new(1),
        });
        let reader_shared = Arc::clone(&shared);
        std::thread::Builder::new()
            .name("acp-reader".into())
            .spawn(move || read_loop(reader, &reader_shared, on_close))?;
        Ok((Self { shared }, rx))
    }

    /// Sends a request and waits for its result.
    pub fn request(&self, method: &str, params: Value, timeout: Duration) -> Result<Value> {
        let id = self.shared.next_id.fetch_add(1, Ordering::Relaxed);
        let (tx, rx) = mpsc::channel();
        lock(&self.shared.pending).insert(id, Pending::Reply(tx));
        let message = json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params });
        if let Err(e) = self.shared.write(&message) {
            lock(&self.shared.pending).remove(&id);
            return Err(e.into());
        }
        match rx.recv_timeout(timeout) {
            Ok(Ok(result)) => Ok(result),
            Ok(Err(e)) => Err(Error::Rpc {
                method: method.to_owned(),
                code: e.code,
                message: e.message,
            }),
            Err(RecvTimeoutError::Timeout) => {
                lock(&self.shared.pending).remove(&id);
                Err(Error::Timeout {
                    method: method.to_owned(),
                })
            }
            Err(RecvTimeoutError::Disconnected) => Err(Error::Disconnected {
                method: method.to_owned(),
            }),
        }
    }

    /// Sends `session/prompt` without waiting; the turn ends with [`AcpEvent::TurnEnded`] or
    /// [`AcpEvent::TurnFailed`].
    pub fn send_prompt(&self, params: Value) -> Result<()> {
        let id = self.shared.next_id.fetch_add(1, Ordering::Relaxed);
        lock(&self.shared.pending).insert(id, Pending::Prompt);
        let message =
            json!({ "jsonrpc": "2.0", "id": id, "method": "session/prompt", "params": params });
        self.shared.write(&message).map_err(|e| {
            lock(&self.shared.pending).remove(&id);
            e.into()
        })
    }

    pub fn notify(&self, method: &str, params: Value) -> Result<()> {
        let message = json!({ "jsonrpc": "2.0", "method": method, "params": params });
        Ok(self.shared.write(&message)?)
    }

    /// Answers a permission request: `Some(option id)` selects it, `None` cancels.
    pub fn answer_permission(&self, request: &RequestId, option_id: Option<&str>) -> Result<()> {
        lock(&self.shared.permissions).remove(&request.key());
        let message = json!({
            "jsonrpc": "2.0",
            "id": request.0,
            "result": protocol::permission_result(option_id),
        });
        Ok(self.shared.write(&message)?)
    }

    /// Answers every unanswered permission request with `cancelled` (required when the client
    /// cancels a turn).
    pub fn cancel_permissions(&self) -> Result<()> {
        let open: Vec<RequestId> = lock(&self.shared.permissions)
            .drain()
            .map(|(_, id)| id)
            .collect();
        for id in open {
            let message = json!({
                "jsonrpc": "2.0",
                "id": id.0,
                "result": protocol::permission_result(None),
            });
            self.shared.write(&message)?;
        }
        Ok(())
    }

    /// Closes the agent's stdin; most agents exit when it ends.
    pub fn close(&self) {
        lock(&self.shared.writer).take();
    }
}

fn read_loop<R: Read, W: Write>(reader: R, shared: &Shared<W>, on_close: OnClose) {
    for line in BufReader::new(reader).lines() {
        let line = match line {
            Ok(line) => line,
            Err(e) => {
                tracing::debug!("acp: read failed: {e}");
                break;
            }
        };
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        match serde_json::from_str::<Value>(line) {
            Ok(message) => handle(shared, message),
            Err(e) => shared.emit(AcpEvent::ProtocolError(format!(
                "not JSON ({e}): {}",
                truncate(line, 200)
            ))),
        }
    }
    // Blocking requests get `Disconnected`; a running prompt turn ends.
    let pending: Vec<Pending> = lock(&shared.pending).drain().map(|(_, p)| p).collect();
    if pending.iter().any(|p| matches!(p, Pending::Prompt)) {
        shared.emit(AcpEvent::TurnFailed(
            "the agent closed the connection".to_owned(),
        ));
    }
    drop(pending);
    lock(&shared.permissions).clear();
    shared.emit(on_close());
}

fn handle<W: Write>(shared: &Shared<W>, message: Value) {
    let method = message["method"].as_str();
    let id = message.get("id").filter(|id| !id.is_null()).cloned();
    match (method, id) {
        (Some(method), Some(id)) => handle_request(shared, method, RequestId(id), &message),
        (Some("session/update"), None) => {
            match protocol::parse_update(&message["params"]["update"]) {
                Some(SessionUpdate::Other { kind }) => {
                    tracing::debug!("acp: ignored session update `{kind}`");
                    shared.emit(AcpEvent::Update(SessionUpdate::Other { kind }));
                }
                Some(update) => shared.emit(AcpEvent::Update(update)),
                None => shared.emit(AcpEvent::ProtocolError(format!(
                    "malformed session/update: {}",
                    truncate(&message["params"].to_string(), 200)
                ))),
            }
        }
        (Some(method), None) => tracing::debug!("acp: ignored notification `{method}`"),
        (None, Some(id)) => handle_response(shared, &id, &message),
        (None, None) => shared.emit(AcpEvent::ProtocolError(format!(
            "message without method or id: {}",
            truncate(&message.to_string(), 200)
        ))),
    }
}

fn handle_request<W: Write>(shared: &Shared<W>, method: &str, id: RequestId, message: &Value) {
    if method == "session/request_permission"
        && let Some((tool_call, options)) = protocol::parse_permission(&message["params"])
    {
        lock(&shared.permissions).insert(id.key(), id.clone());
        shared.emit(AcpEvent::PermissionRequest(PermissionRequest {
            request_id: id,
            tool_call,
            options,
        }));
        return;
    }
    let (code, text) = if method == "session/request_permission" {
        (protocol::INVALID_PARAMS, "malformed permission request")
    } else {
        // We advertise no fs or terminal capability, so the agent should not ask for them.
        (METHOD_NOT_FOUND, "method not supported by Hangar")
    };
    tracing::debug!("acp: rejected agent request `{method}`");
    let reply = json!({
        "jsonrpc": "2.0",
        "id": id.0,
        "error": { "code": code, "message": format!("{text}: {method}") },
    });
    if let Err(e) = shared.write(&reply) {
        tracing::debug!("acp: reply failed: {e}");
    }
}

fn handle_response<W: Write>(shared: &Shared<W>, id: &Value, message: &Value) {
    let Some(pending) = id.as_u64().and_then(|id| lock(&shared.pending).remove(&id)) else {
        shared.emit(AcpEvent::ProtocolError(format!(
            "response to unknown request {id}"
        )));
        return;
    };
    let outcome = match message.get("error") {
        Some(error) => Err(RpcError {
            code: error["code"].as_i64().unwrap_or_default(),
            message: error["message"].as_str().unwrap_or("error").to_owned(),
        }),
        None => Ok(message["result"].clone()),
    };
    match pending {
        Pending::Reply(tx) => {
            let _ = tx.send(outcome);
        }
        Pending::Prompt => match outcome {
            Ok(result) => {
                let reason = result["stopReason"].as_str().unwrap_or_default();
                match StopReason::parse(reason) {
                    Some(reason) => shared.emit(AcpEvent::TurnEnded(reason)),
                    None => shared.emit(AcpEvent::TurnFailed(format!(
                        "unknown stop reason `{reason}`"
                    ))),
                }
            }
            Err(e) => shared.emit(AcpEvent::TurnFailed(e.message)),
        },
    }
}

fn truncate(s: &str, max: usize) -> &str {
    match s.char_indices().nth(max) {
        Some((i, _)) => &s[..i],
        None => s,
    }
}

#[cfg(test)]
mod tests {
    use std::io::{PipeReader, PipeWriter, pipe};

    use super::*;

    /// The agent's end of the pipes: reads what the client wrote, writes what it reads.
    struct FakeAgent {
        from_client: BufReader<PipeReader>,
        to_client: Option<PipeWriter>,
    }

    impl FakeAgent {
        fn recv(&mut self) -> Value {
            let mut line = String::new();
            self.from_client.read_line(&mut line).expect("read");
            serde_json::from_str(&line).expect("json")
        }

        fn send(&mut self, v: Value) {
            let w = self.to_client.as_mut().expect("open");
            writeln!(w, "{v}").expect("write");
        }

        fn close(&mut self) {
            self.to_client = None;
        }
    }

    fn connect() -> (AcpConnection<PipeWriter>, Receiver<AcpEvent>, FakeAgent) {
        let (client_read, agent_write) = pipe().expect("pipe");
        let (agent_read, client_write) = pipe().expect("pipe");
        let (conn, rx) = AcpConnection::start(
            client_read,
            client_write,
            None,
            Box::new(|| AcpEvent::Exited {
                code: Some(0),
                stderr_tail: String::new(),
            }),
        )
        .expect("start");
        let agent = FakeAgent {
            from_client: BufReader::new(agent_read),
            to_client: Some(agent_write),
        };
        (conn, rx, agent)
    }

    fn next(rx: &Receiver<AcpEvent>) -> AcpEvent {
        rx.recv_timeout(Duration::from_secs(5)).expect("event")
    }

    #[test]
    fn request_gets_its_result() {
        let (conn, _rx, mut agent) = connect();
        let t = std::thread::spawn(move || {
            let req = agent.recv();
            assert_eq!(req["method"], "initialize");
            assert_eq!(req["params"]["protocolVersion"], 1);
            agent
                .send(json!({"jsonrpc": "2.0", "id": req["id"], "result": {"protocolVersion": 1}}));
            let req = agent.recv();
            agent.send(json!({"jsonrpc": "2.0", "id": req["id"], "error": {"code": -32000, "message": "Authentication required"}}));
            agent
        });
        let result = conn
            .request(
                "initialize",
                protocol::initialize_params(),
                Duration::from_secs(5),
            )
            .expect("result");
        assert_eq!(result["protocolVersion"], 1);
        let err = conn
            .request("session/new", json!({}), Duration::from_secs(5))
            .expect_err("rpc error");
        assert!(matches!(err, Error::Rpc { code: -32000, .. }), "{err:?}");
        t.join().expect("agent");
    }

    #[test]
    fn prompt_turn_streams_then_ends() {
        let (conn, rx, mut agent) = connect();
        conn.send_prompt(protocol::prompt_params("s1", "hello"))
            .expect("send");
        let req = agent.recv();
        assert_eq!(req["method"], "session/prompt");
        assert_eq!(req["params"]["prompt"][0]["text"], "hello");
        agent.send(json!({"jsonrpc": "2.0", "method": "session/update", "params": {"sessionId": "s1",
            "update": {"sessionUpdate": "agent_message_chunk", "content": {"type": "text", "text": "Hi"}}}}));
        agent.send(
            json!({"jsonrpc": "2.0", "method": "session/update", "params": {"sessionId": "s1",
            "update": {"sessionUpdate": "brand_new"}}}),
        );
        agent
            .send(json!({"jsonrpc": "2.0", "id": req["id"], "result": {"stopReason": "end_turn"}}));
        assert_eq!(
            next(&rx),
            AcpEvent::Update(SessionUpdate::AgentMessage {
                text: "Hi".into(),
                parent: None
            })
        );
        assert_eq!(
            next(&rx),
            AcpEvent::Update(SessionUpdate::Other {
                kind: "brand_new".into()
            })
        );
        assert_eq!(next(&rx), AcpEvent::TurnEnded(StopReason::EndTurn));
    }

    #[test]
    fn permission_round_trip_and_cancel() {
        let (conn, rx, mut agent) = connect();
        let ask = |agent: &mut FakeAgent, id: Value| {
            agent.send(
                json!({"jsonrpc": "2.0", "id": id, "method": "session/request_permission",
                "params": {"sessionId": "s1", "toolCall": {"toolCallId": "t1", "title": "Write"},
                "options": [{"optionId": "ok", "name": "Allow", "kind": "allow_once"}]}}),
            );
        };
        ask(&mut agent, json!(7));
        let AcpEvent::PermissionRequest(request) = next(&rx) else {
            panic!("expected a permission request");
        };
        assert_eq!(request.options[0].id, "ok");
        conn.answer_permission(&request.request_id, Some("ok"))
            .expect("answer");
        assert_eq!(
            agent.recv(),
            json!({"jsonrpc": "2.0", "id": 7, "result": {"outcome": {"outcome": "selected", "optionId": "ok"}}})
        );

        ask(&mut agent, json!("p-2"));
        let AcpEvent::PermissionRequest(_) = next(&rx) else {
            panic!("expected a permission request");
        };
        conn.cancel_permissions().expect("cancel");
        assert_eq!(
            agent.recv(),
            json!({"jsonrpc": "2.0", "id": "p-2", "result": {"outcome": {"outcome": "cancelled"}}})
        );
    }

    #[test]
    fn unsupported_requests_and_garbage() {
        let (_conn, rx, mut agent) = connect();
        agent.send(json!({"jsonrpc": "2.0", "id": 1, "method": "fs/read_text_file", "params": {}}));
        let reply = agent.recv();
        assert_eq!(reply["id"], 1);
        assert_eq!(reply["error"]["code"], METHOD_NOT_FOUND);
        let w = agent.to_client.as_mut().expect("open");
        writeln!(w, "{{not json").expect("write");
        assert!(matches!(next(&rx), AcpEvent::ProtocolError(_)));
    }

    #[test]
    fn close_ends_turn_and_fails_waiters() {
        let (conn, rx, mut agent) = connect();
        conn.send_prompt(protocol::prompt_params("s1", "x"))
            .expect("send");
        let _ = agent.recv();
        let t = std::thread::spawn(move || {
            let _ = agent.recv();
            agent.close();
        });
        let err = conn
            .request("session/new", json!({}), Duration::from_secs(5))
            .expect_err("disconnected");
        assert!(matches!(err, Error::Disconnected { .. }), "{err:?}");
        t.join().expect("agent");
        assert!(matches!(next(&rx), AcpEvent::TurnFailed(_)));
        assert!(matches!(next(&rx), AcpEvent::Exited { code: Some(0), .. }));
    }
}
