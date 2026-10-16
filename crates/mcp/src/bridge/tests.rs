use super::*;
use crate::protocol::tests::{FakeHost, SESSION};
use crate::protocol::{INVALID_REQUEST, UNKNOWN_SESSION};
use crate::server::{bind, serve};
use std::io::Cursor;

fn lines(messages: &[Value]) -> Cursor<Vec<u8>> {
    let mut input = String::new();
    for m in messages {
        input.push_str(&m.to_string());
        input.push('\n');
    }
    Cursor::new(input.into_bytes())
}

fn responses(output: &[u8]) -> HashMap<String, Value> {
    String::from_utf8_lossy(output)
        .lines()
        .map(|l| serde_json::from_str::<Value>(l).unwrap())
        .map(|v| (v["id"].to_string(), v))
        .collect()
}

fn request(id: i64, method: &str, params: Value) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params })
}

fn requests() -> Vec<Value> {
    vec![
        request(
            1,
            "initialize",
            json!({ "protocolVersion": "2025-03-26", "capabilities": {} }),
        ),
        json!({ "jsonrpc": "2.0", "method": "notifications/initialized" }),
        request(2, "tools/list", json!({})),
        request(
            3,
            "tools/call",
            json!({ "name": "tap", "arguments": { "x": 5, "y": 6 } }),
        ),
        request(
            4,
            "tools/call",
            json!({ "name": "screenshot", "arguments": {} }),
        ),
        request(5, "tools/call", json!({ "name": "tap", "arguments": {} })),
    ]
}

fn start_server(dir: &Path) -> (std::path::PathBuf, Arc<FakeHost>) {
    let path = dir.join("mcp.sock");
    let listener = bind(&path).unwrap();
    let host = Arc::new(FakeHost::default());
    let served: Arc<dyn crate::ToolHost> = host.clone();
    std::thread::spawn(move || serve(listener, served));
    (path, host)
}

#[test]
fn end_to_end_through_socket() {
    let dir = tempfile::tempdir().unwrap();
    let (path, host) = start_server(dir.path());
    let mut output = Vec::new();
    run_bridge(&path, SESSION, lines(&requests()), &mut output).unwrap();

    let r = responses(&output);
    assert_eq!(r.len(), 5, "{r:?}");
    assert_eq!(r["1"]["result"]["protocolVersion"], "2025-03-26");
    assert_eq!(r["2"]["result"]["tools"].as_array().map(Vec::len), Some(12));
    assert_eq!(r["3"]["result"]["content"][0]["text"], "ok tap");
    assert_eq!(r["4"]["result"]["content"][0]["data"], "Zm9v");
    assert_eq!(r["5"]["result"]["isError"], true);
    assert_eq!(host.calls.lock().unwrap().len(), 2);
}

#[test]
fn wrong_session_is_rejected_by_the_app() {
    let dir = tempfile::tempdir().unwrap();
    let (path, host) = start_server(dir.path());
    let mut output = Vec::new();
    run_bridge(&path, "stolen", lines(&requests()), &mut output).unwrap();
    let r = responses(&output);
    assert_eq!(r.len(), 5);
    assert!(r.values().all(|v| v["error"]["code"] == UNKNOWN_SESSION));
    assert!(host.calls.lock().unwrap().is_empty());
}

#[test]
fn app_not_running_answers_every_request() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("absent.sock");
    let mut input = requests();
    input.push(json!({ "jsonrpc": "2.0", "id": 9, "result": {} }));
    let mut output = Vec::new();
    let mut cursor = lines(&input);
    cursor.get_mut().extend_from_slice(b"{oops\n");
    run_bridge(&path, SESSION, cursor, &mut output).unwrap();

    let r = responses(&output);
    assert_eq!(r.len(), 6, "{r:?}");
    for id in 1..=5 {
        let e = &r[&id.to_string()]["error"];
        assert_eq!(e["code"], UNAVAILABLE);
        assert!(
            e["message"]
                .as_str()
                .unwrap()
                .contains("Hangar is not reachable")
        );
    }
    assert_eq!(r["null"]["error"]["code"], PARSE_ERROR);
}

#[test]
fn app_dropping_the_connection_fails_pending_requests() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("mcp.sock");
    let listener = bind(&path).unwrap();
    std::thread::spawn(move || {
        let (stream, _) = listener.accept().unwrap();
        let mut line = String::new();
        BufReader::new(&stream).read_line(&mut line).unwrap();
        let envelope: Value = serde_json::from_str(&line).unwrap();
        assert_eq!(envelope["session"], SESSION);
        assert_eq!(envelope["message"]["method"], "tools/call");
    });
    let mut output = Vec::new();
    let input = lines(&[request(1, "tools/call", json!({ "name": "build_and_run" }))]);
    run_bridge(&path, SESSION, input, &mut output).unwrap();
    let r = responses(&output);
    assert_eq!(r["1"]["error"]["code"], UNAVAILABLE);
    assert!(
        r["1"]["error"]["message"]
            .as_str()
            .unwrap()
            .contains("closed the connection")
    );
}

#[test]
fn ids_of_requests_and_responses() {
    let batch = json!([
        { "jsonrpc": "2.0", "id": 1, "method": "ping" },
        { "jsonrpc": "2.0", "method": "notifications/initialized" },
        { "jsonrpc": "2.0", "id": "x", "result": {} },
    ]);
    assert_eq!(request_ids(&batch), vec![json!(1)]);
    assert_eq!(response_ids(&batch), vec![json!("x")]);
    let invalid = error_response(&Value::Null, INVALID_REQUEST, "x");
    assert_eq!(response_ids(&invalid), vec![Value::Null]);
}
