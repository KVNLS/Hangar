use super::*;
use crate::mcp_host::fixtures::{Log, fake_consumer, host, orchestration_host, short, text};
use std::time::Instant;

#[test]
fn sessions_register_resolve_unregister() {
    let s = Sessions::default();
    let (a, b) = (WorkspaceId::new(), WorkspaceId::new());
    let sa = s.register(a);
    assert_eq!(s.register(a), sa, "register is idempotent");
    let sb = s.register(b);
    assert_ne!(sa, sb);
    assert_eq!(s.resolve(&sa), Some(a));
    assert_eq!(s.resolve(&sb), Some(b));
    assert_eq!(s.resolve(&sa[..63]), None);
    assert_eq!(s.resolve(""), None);
    s.unregister(a);
    assert_eq!(s.resolve(&sa), None);
    assert_eq!(s.resolve(&sb), Some(b));
    assert!(ct_eq(b"abc", b"abc"));
    assert!(!ct_eq(b"abc", b"abd"));
    assert!(!ct_eq(b"abc", b"ab"));
}

#[test]
fn unknown_session_is_rejected_without_a_request() {
    let (host, _sessions, mut rx) = host(short());
    assert!(!host.knows_session("nope"));
    assert_eq!(
        host.call("nope", Tool::DeviceInfo),
        Err(ToolError::UnknownSession)
    );
    assert!(rx.try_recv().is_err());
}

#[test]
fn errors_timeouts_and_closed_channels() {
    let (host, sessions, rx) = host(short());
    fake_consumer(rx, Log::default());
    let s = sessions.register(WorkspaceId::new());

    assert_eq!(
        host.call(
            &s,
            Tool::Press(hangar_mcp::Press::Button(hangar_mcp::Button::Back))
        ),
        Err(ToolError::Unsupported("Back on iOS".into()))
    );
    assert_eq!(
        host.call(&s, Tool::LaunchApp { app_id: "x".into() }),
        Err(ToolError::Failed("not installed".into()))
    );
    let started = Instant::now();
    let err = host.call(
        &s,
        Tool::LaunchApp {
            app_id: "hang".into(),
        },
    );
    assert_eq!(
        err,
        Err(ToolError::Failed(
            "launch_app timed out after 200 ms".into()
        ))
    );
    assert!(started.elapsed() < Duration::from_secs(1));
    assert!(matches!(
        host.call(&s, Tool::LaunchApp { app_id: "drop".into() }),
        Err(ToolError::Failed(m)) if m.contains("abandoned")
    ));
    // build_and_run uses its own, longer timeout (the fake answers after 400 ms).
    let out = host.call(&s, Tool::BuildAndRun).unwrap();
    assert_eq!(text(&out), "Launched com.example.app");

    let (host, sessions, rx) = fixtures::host(short());
    drop(rx);
    let s = sessions.register(WorkspaceId::new());
    assert!(matches!(
        host.call(&s, Tool::Tap { x: 1.0, y: 1.0 }),
        Err(ToolError::Failed(m)) if m.contains("shutting down")
    ));
}

#[test]
fn start_serves_and_cleans_up() {
    let dir = tempfile::tempdir().unwrap();
    let socket = dir.path().join("hangar.sock");
    let (handle, rx) = start_with(&socket, short()).unwrap();
    fake_consumer(rx, Log::default());
    assert_eq!(handle.socket_path(), socket);
    let ws = WorkspaceId::new();
    let secret = handle.register(ws);

    let input = [
        r#"{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"device_info"}}"#,
        r#"{"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"tap","arguments":{"x":1,"y":2}}}"#,
    ]
    .join("\n");
    let mut out = Vec::new();
    hangar_mcp::run_bridge(&socket, &secret, input.as_bytes(), &mut out).unwrap();
    let out = String::from_utf8(out).unwrap();
    let responses: Vec<serde_json::Value> = out
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect();
    assert_eq!(responses.len(), 2, "{out}");
    for r in &responses {
        assert_eq!(
            r["result"]["isError"],
            serde_json::Value::Bool(false),
            "{r}"
        );
    }

    handle.unregister(ws);
    let mut out = Vec::new();
    hangar_mcp::run_bridge(&socket, &secret, input.as_bytes(), &mut out).unwrap();
    assert!(String::from_utf8(out).unwrap().contains("-32001"));

    drop(handle);
    assert!(!socket.exists());
}

#[test]
fn agent_signals_go_to_the_workspace_without_waiting() {
    let (host, sessions, mut rx) = host(short());
    let ws = WorkspaceId::new();
    let s = sessions.register(ws);
    host.agent_signal(&s, hangar_core::AgentSignal::Approval, None)
        .unwrap();
    match rx.try_recv() {
        Ok(McpRequest::Signal {
            ws: got, signal, ..
        }) => {
            assert_eq!((got, signal), (ws, hangar_core::AgentSignal::Approval));
        }
        other => panic!("expected a signal, got {:?}", other.map(|r| r.workspace())),
    }
    assert_eq!(
        host.agent_signal("nope", hangar_core::AgentSignal::Working, None),
        Err(ToolError::UnknownSession)
    );
}

#[test]
fn ask_user_waits_for_the_answer() {
    let (host, sessions, rx) = host(short());
    let log = Log::default();
    fake_consumer(rx, log.clone());
    let s = sessions.register(WorkspaceId::new());
    let out = host
        .call(
            &s,
            Tool::AskUser {
                question: "REST or GraphQL?".into(),
                choices: vec!["REST".into(), "GraphQL".into()],
            },
        )
        .unwrap();
    assert_eq!(text(&out), r#"{"answer":"REST","choice":0}"#);
    assert!(log.lock().unwrap()[0].ends_with(r#"ask REST or GraphQL? ["REST", "GraphQL"]"#));
}

#[test]
fn an_unanswered_question_times_out() {
    // Nobody consumes the requests: the question is never answered.
    let (host, sessions, _rx) = host(short());
    let s = sessions.register(WorkspaceId::new());
    let err = host
        .call(
            &s,
            Tool::AskUser {
                question: "Still there?".into(),
                choices: vec![],
            },
        )
        .unwrap_err();
    assert!(err.to_string().contains("ask_user timed out"), "{err}");
}
#[test]
fn orchestration_tools_are_refused_while_off() {
    let (host, sessions, mut rx, on) = orchestration_host(short());
    let s = sessions.register(WorkspaceId::new());
    assert!(!host.orchestration_enabled(&s));
    assert!(matches!(
        host.call(&s, Tool::WorkspacesList),
        Err(ToolError::Failed(m)) if m.contains("off")
    ));
    assert!(rx.try_recv().is_err());
    on.store(true, Ordering::Relaxed);
    assert!(host.orchestration_enabled(&s));
    assert!(!host.orchestration_enabled("stranger"));
}

#[test]
fn only_jsonl_files_inside_the_projects_directory_are_accepted() {
    let home = tempfile::tempdir().unwrap();
    let root = home.path().join(".claude/projects");
    let project = root.join("-repo");
    std::fs::create_dir_all(&project).unwrap();
    let good = project.join("s.jsonl");
    std::fs::write(&good, "{}").unwrap();
    let outside = home.path().join("secret.jsonl");
    std::fs::write(&outside, "{}").unwrap();
    let not_jsonl = project.join("notes.txt");
    std::fs::write(&not_jsonl, "x").unwrap();
    let link = project.join("escape.jsonl");
    std::os::unix::fs::symlink(&outside, &link).unwrap();

    assert_eq!(
        accept_transcript(&good, &root),
        Some(good.canonicalize().unwrap())
    );
    for bad in [
        outside.clone(),
        not_jsonl,
        link,
        project.join("../../secret.jsonl"),
        project.join("missing.jsonl"),
    ] {
        assert_eq!(accept_transcript(&bad, &root), None, "{}", bad.display());
    }
}
