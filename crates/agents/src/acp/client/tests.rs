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
        agent.send(json!({"jsonrpc": "2.0", "id": req["id"], "result": {"protocolVersion": 1}}));
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
    agent.send(json!({"jsonrpc": "2.0", "id": req["id"], "result": {"stopReason": "end_turn"}}));
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
