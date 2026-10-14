//! End to end: the built `Hangar` binary in `mcp-bridge` mode talking to an `McpHost` whose
//! requests are answered by a fake GPUI side.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::io::Write as _;
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::time::Duration;

use hangar_app::mcp_host::{McpRequest, ScreenFrame, Timeouts, start_with};
use hangar_core::WorkspaceId;
use hangar_devices::Frame;
use serde_json::Value;

fn fake_gpui_side(mut rx: tokio::sync::mpsc::UnboundedReceiver<McpRequest>) {
    std::thread::spawn(move || {
        while let Some(req) = rx.blocking_recv() {
            match req {
                McpRequest::Frame { reply, .. } => {
                    let (w, h) = (1206u32, 2622u32);
                    let bgra = Arc::from(vec![128u8; w as usize * h as usize * 4]);
                    let frame = Frame::bgra(w, h, w as usize * 4, bgra, 1);
                    let _ = reply.send(Ok(ScreenFrame {
                        frame,
                        width_pt: 402.0,
                        height_pt: 874.0,
                    }));
                }
                McpRequest::Children { reply, .. } => {
                    let _ = reply.send(Ok(vec![hangar_mcp::ChildWorkspace {
                        id: "child".into(),
                        name: "Login".into(),
                        task: Some("Fix login".into()),
                        branch: "hangar/login".into(),
                        agent: Some("Codex".into()),
                        state: "Working".into(),
                        diff: Some((3, 1)),
                        queued: false,
                    }]));
                }
                other => drop(other),
            }
        }
    });
}

fn bridge(socket: &std::path::Path, session: &str, input: &str) -> (Vec<Value>, String, bool) {
    let mut child = Command::new(env!("CARGO_BIN_EXE_Hangar"))
        .arg("mcp-bridge")
        .env("HANGAR_MCP_SOCKET", socket)
        .env("HANGAR_MCP_SESSION", session)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(input.as_bytes())
        .unwrap();
    let out = child.wait_with_output().unwrap();
    let stdout = String::from_utf8(out.stdout).unwrap();
    let responses = stdout
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect();
    (
        responses,
        String::from_utf8_lossy(&out.stderr).into_owned(),
        out.status.success(),
    )
}

fn by_id(responses: &[Value], id: u64) -> &Value {
    responses
        .iter()
        .find(|r| r["id"] == id)
        .unwrap_or_else(|| panic!("no response {id} in {responses:?}"))
}

#[test]
fn bridge_binary_reaches_the_host() {
    let dir = tempfile::tempdir().unwrap();
    let socket = dir.path().join("hangar.sock");
    let timeouts = Timeouts {
        call: Duration::from_secs(5),
        build_and_run: Duration::from_secs(5),
        create: Duration::from_secs(5),
        ask: Duration::from_secs(5),
    };
    let (handle, rx) = start_with(&socket, timeouts).unwrap();
    fake_gpui_side(rx);
    let secret = handle.register(WorkspaceId::new());

    let input = [
        r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-06-18","capabilities":{},"clientInfo":{"name":"e2e","version":"0"}}}"#,
        r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#,
        r#"{"jsonrpc":"2.0","id":2,"method":"tools/list"}"#,
        r#"{"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"screenshot","arguments":{}}}"#,
        "",
    ]
    .join("\n");
    let (responses, stderr, ok) = bridge(&socket, &secret, &input);
    assert!(ok, "bridge failed: {stderr}");
    assert_eq!(responses.len(), 3, "{responses:?}");

    let init = by_id(&responses, 1);
    println!(
        "initialize -> protocolVersion {} server {}",
        init["result"]["protocolVersion"], init["result"]["serverInfo"]
    );
    assert_eq!(init["result"]["protocolVersion"], "2025-06-18");

    let tools: Vec<&str> = by_id(&responses, 2)["result"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|t| t["name"].as_str())
        .collect();
    println!("tools/list -> {tools:?}");
    assert!(tools.contains(&"screenshot") && tools.contains(&"build_and_run"));

    let shot = &by_id(&responses, 3)["result"];
    assert_eq!(shot["isError"], false, "{shot}");
    let content = shot["content"].as_array().unwrap();
    assert_eq!(content[0]["type"], "image");
    assert_eq!(content[0]["mimeType"], "image/png");
    let b64 = content[0]["data"].as_str().unwrap();
    assert!(b64.starts_with("iVBORw0KGgo"), "PNG signature in base64");
    println!(
        "tools/call screenshot -> image/png ({} base64 chars), caption: {}",
        b64.len(),
        content[1]["text"]
    );

    let wrong = r#"{"jsonrpc":"2.0","id":4,"method":"tools/call","params":{"name":"screenshot"}}"#;
    let (responses, stderr, ok) = bridge(&socket, "0000wrong", &format!("{wrong}\n"));
    assert!(ok, "bridge failed: {stderr}");
    let err = &by_id(&responses, 4)["error"];
    println!("wrong session -> {err}");
    assert_eq!(err["code"], -32001);

    drop(handle);
    assert!(!socket.exists());
    let (responses, _, ok) = bridge(&socket, &secret, &format!("{wrong}\n"));
    assert!(ok);
    println!("app gone -> {}", by_id(&responses, 4)["error"]);
    assert!(
        by_id(&responses, 4)["error"]["message"]
            .as_str()
            .unwrap()
            .contains("not reachable")
    );
}

#[test]
fn orchestration_tools_follow_the_setting() {
    let dir = tempfile::tempdir().unwrap();
    let socket = dir.path().join("hangar.sock");
    let (handle, rx) = start_with(&socket, Timeouts::default()).unwrap();
    fake_gpui_side(rx);
    let secret = handle.register(WorkspaceId::new());
    let input = [
        r#"{"jsonrpc":"2.0","id":1,"method":"tools/list"}"#,
        r#"{"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"workspaces_list","arguments":{}}}"#,
        "",
    ]
    .join("\n");
    let names = |responses: &[Value]| -> Vec<String> {
        by_id(responses, 1)["result"]["tools"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|t| t["name"].as_str().map(str::to_owned))
            .collect()
    };

    let (responses, stderr, ok) = bridge(&socket, &secret, &input);
    assert!(ok, "bridge failed: {stderr}");
    assert!(!names(&responses).contains(&"workspaces_list".to_owned()));
    let refused = &by_id(&responses, 2)["result"];
    println!("off -> {refused}");
    assert_eq!(refused["isError"], true);

    handle.set_orchestration(true);
    let (responses, stderr, ok) = bridge(&socket, &secret, &input);
    assert!(ok, "bridge failed: {stderr}");
    let listed = names(&responses);
    println!("on -> tools/list {listed:?}");
    for tool in [
        "workspaces_list",
        "workspace_create",
        "agent_prompt",
        "agent_transcript",
    ] {
        assert!(listed.contains(&tool.to_owned()), "{tool}");
    }
    let result = &by_id(&responses, 2)["result"];
    assert_eq!(result["isError"], false, "{result}");
    let text: Value = serde_json::from_str(result["content"][0]["text"].as_str().unwrap()).unwrap();
    println!("on -> workspaces_list {text}");
    assert_eq!(text["workspaces"][0]["diff"]["added"], 3);
}
