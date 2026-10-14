use super::*;
use hangar_devices::Platform;
use hangar_mcp::Content;
use std::sync::Arc;
use std::time::Instant;

type Log = Arc<Mutex<Vec<String>>>;

fn host(timeouts: Timeouts) -> (McpHost, Sessions, mpsc::UnboundedReceiver<McpRequest>) {
    let (host, sessions, rx, _) = orchestration_host(timeouts);
    (host, sessions, rx)
}

/// As [`host`], with the switch of the orchestration tools (off).
fn orchestration_host(
    timeouts: Timeouts,
) -> (
    McpHost,
    Sessions,
    mpsc::UnboundedReceiver<McpRequest>,
    Arc<AtomicBool>,
) {
    let (tx, rx) = mpsc::unbounded_channel();
    let sessions = Sessions::default();
    let on = Arc::new(AtomicBool::new(false));
    (
        McpHost::new(sessions.clone(), tx, timeouts, Arc::clone(&on)).unwrap(),
        sessions,
        rx,
        on,
    )
}

fn short() -> Timeouts {
    Timeouts {
        call: Duration::from_millis(200),
        build_and_run: Duration::from_millis(800),
        create: Duration::from_millis(800),
        ask: Duration::from_millis(800),
    }
}

fn frame(width: u32, height: u32) -> Frame {
    let bgra = Arc::from(vec![200u8; width as usize * height as usize * 4]);
    Frame::bgra(width, height, width as usize * 4, bgra, 1)
}

/// Answers like a device would, recording each request.
fn fake_consumer(mut rx: mpsc::UnboundedReceiver<McpRequest>, log: Log) {
    std::thread::spawn(move || {
        while let Some(req) = rx.blocking_recv() {
            let ws = req.workspace();
            let record = |s: String| log.lock().unwrap().push(format!("{ws} {s}"));
            match req {
                McpRequest::Status { reply, .. } => {
                    record("status".into());
                    let _ = reply.send(Ok(DeviceStatus {
                        platform: Platform::Ios,
                        name: "iPhone".into(),
                        id: "UDID".into(),
                        booted: true,
                        screen_pt: Some((402.0, 874.0)),
                        scale: Some(3.0),
                        orientation: None,
                    }));
                }
                McpRequest::Frame { reply, .. } => {
                    record("frame".into());
                    let _ = reply.send(Ok(ScreenFrame {
                        frame: frame(1206, 2622),
                        width_pt: 402.0,
                        height_pt: 874.0,
                    }));
                }
                McpRequest::Input { input, reply, .. } => {
                    record(format!("{input:?}"));
                    let result = match input {
                        Input::Back => Err(anyhow::Error::new(hangar_devices::Error::Unsupported(
                            "Back on iOS",
                        ))
                        .context("press")),
                        _ => Ok(()),
                    };
                    let _ = reply.send(result);
                }
                McpRequest::Ask {
                    question,
                    choices,
                    reply,
                    ..
                } => {
                    record(format!("ask {question} {choices:?}"));
                    let _ = reply.send(Ok(hangar_mcp::Answer {
                        text: choices.first().cloned().unwrap_or_else(|| "typed".into()),
                        choice: (!choices.is_empty()).then_some(0),
                    }));
                }
                McpRequest::Signal { signal, .. } => record(format!("signal {}", signal.as_str())),
                McpRequest::Text { text, reply, .. } => {
                    record(format!("text {text}"));
                    let _ = reply.send(Ok(()));
                }
                McpRequest::Logs {
                    lines,
                    min_level,
                    process,
                    reply,
                    ..
                } => {
                    record(format!("logs {lines} {min_level:?} {process:?}"));
                    let mut e = LogEntry::new(LogLevel::Warn, "low memory");
                    e.tag = "UIKit".into();
                    e.process = "MyApp".into();
                    e.pid = Some(42);
                    let _ = reply.send(Ok(vec![e]));
                }
                McpRequest::BuildAndRun { reply, .. } => {
                    record("build".into());
                    std::thread::spawn(move || {
                        std::thread::sleep(Duration::from_millis(400));
                        let _ = reply.send(Ok("Launched com.example.app".into()));
                    });
                }
                McpRequest::Launch { app_id, reply, .. } => {
                    record(format!("launch {app_id}"));
                    if app_id == "hang" {
                        std::thread::spawn(move || {
                            std::thread::sleep(Duration::from_secs(2));
                            drop(reply);
                        });
                    } else if app_id == "drop" {
                        drop(reply);
                    } else {
                        let _ = reply.send(Err(anyhow::anyhow!("not installed")));
                    }
                }
                McpRequest::Children { reply, .. } => {
                    record("children".into());
                    let _ = reply.send(Ok(vec![ChildWorkspace {
                        id: "c1".into(),
                        name: "Login".into(),
                        task: Some("Fix login".into()),
                        branch: "hangar/login".into(),
                        agent: Some("Codex".into()),
                        state: "Working".into(),
                        diff: None,
                        queued: false,
                    }]));
                }
                McpRequest::CreateChild {
                    task,
                    name,
                    agent,
                    reply,
                    ..
                } => {
                    record(format!("create {task} {name:?} {agent:?}"));
                    // Longer than a plain call: creates have their own timeout.
                    std::thread::spawn(move || {
                        std::thread::sleep(Duration::from_millis(400));
                        let _ = reply.send(Ok(CreatedWorkspace {
                            id: "c2".into(),
                            name: "Docs".into(),
                            branch: "hangar/docs".into(),
                            agent: "Claude Code".into(),
                            start: hangar_mcp::StartOutcome::Queued { ahead: 1 },
                        }));
                    });
                }
                McpRequest::PromptChild {
                    child, text, reply, ..
                } => {
                    record(format!("prompt {child} {text}"));
                    let _ = reply.send(match child.as_str() {
                        "Login" => Ok("Login".into()),
                        _ => Err(anyhow::Error::new(ToolError::Failed(format!(
                            "no workspace {child:?}"
                        )))),
                    });
                }
                McpRequest::Transcript {
                    child, last, reply, ..
                } => {
                    record(format!("transcript {child} {last}"));
                    let _ = reply.send(match child.as_str() {
                        "Login" => Ok((
                            "Login".into(),
                            vec![
                                TranscriptEntry::User("Fix login".into()),
                                TranscriptEntry::Agent("Done.".into()),
                            ],
                        )),
                        _ => Err(anyhow::Error::new(ToolError::Unsupported(
                            "transcripts of terminal agents".into(),
                        ))),
                    });
                }
            }
        }
    });
}

fn text(out: &ToolOutput) -> String {
    out.content
        .iter()
        .filter_map(|c| match c {
            Content::Text(t) => Some(t.clone()),
            Content::Png(_) => None,
        })
        .collect()
}

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
fn tools_map_to_requests() {
    let (host, sessions, rx) = host(short());
    let log = Log::default();
    fake_consumer(rx, log.clone());
    let ws = WorkspaceId::new();
    let s = sessions.register(ws);
    assert!(host.knows_session(&s));

    let out = host.call(&s, Tool::DeviceInfo).unwrap();
    assert!(text(&out).contains("\"width_pt\": 402.0"), "{}", text(&out));
    host.call(&s, Tool::Tap { x: 10.0, y: 20.5 }).unwrap();
    host.call(&s, Tool::TypeText { text: "hé".into() }).unwrap();
    let out = host
        .call(
            &s,
            Tool::Press(hangar_mcp::Press::Button(hangar_mcp::Button::Home)),
        )
        .unwrap();
    assert_eq!(text(&out), "Pressed.");
    let out = host
        .call(
            &s,
            Tool::Logs {
                lines: 5,
                min_level: Some(LogLevel::Warn),
                process: Some("MyApp".into()),
            },
        )
        .unwrap();
    assert_eq!(text(&out), "warn MyApp UIKit[42]: low memory\n");

    let log = log.lock().unwrap();
    assert_eq!(
        *log,
        vec![
            format!("{ws} status"),
            format!("{ws} Tap {{ x: 10.0, y: 20.5 }}"),
            format!("{ws} text hé"),
            format!("{ws} Home"),
            format!("{ws} logs 5 Some(Warn) Some(\"MyApp\")"),
        ]
    );
}

#[test]
fn swipe_expands_to_touch_events() {
    let steps = swipe_inputs((0.0, 100.0), (0.0, 0.0), 64);
    assert_eq!(steps.len(), 6);
    assert_eq!(
        steps[0],
        (Input::TouchDown { x: 0.0, y: 100.0 }, Duration::ZERO)
    );
    assert_eq!(
        steps[1],
        (
            Input::TouchMove { x: 0.0, y: 75.0 },
            Duration::from_millis(16)
        )
    );
    assert_eq!(steps[4].0, Input::TouchMove { x: 0.0, y: 0.0 });
    assert_eq!(
        steps[5],
        (Input::TouchUp { x: 0.0, y: 0.0 }, Duration::ZERO)
    );

    let instant = swipe_inputs((1.0, 1.0), (2.0, 2.0), 0);
    assert_eq!(instant.len(), 3);
    assert!(instant.iter().all(|(_, d)| d.is_zero()));
    assert_eq!(swipe_inputs((0.0, 0.0), (1.0, 1.0), 10_000).len(), 122);

    let (host, sessions, rx) = host(short());
    let log = Log::default();
    fake_consumer(rx, log.clone());
    let s = sessions.register(WorkspaceId::new());
    let started = Instant::now();
    host.call(
        &s,
        Tool::Swipe {
            from_x: 0.0,
            from_y: 100.0,
            to_x: 0.0,
            to_y: 0.0,
            duration_ms: 64,
        },
    )
    .unwrap();
    assert!(started.elapsed() >= Duration::from_millis(64));
    let log = log.lock().unwrap();
    assert_eq!(log.len(), 6);
    assert!(log[0].ends_with("TouchDown { x: 0.0, y: 100.0 }"));
    assert!(log[5].ends_with("TouchUp { x: 0.0, y: 0.0 }"));
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

    let (host, sessions, rx) = super::tests::host(short());
    drop(rx);
    let s = sessions.register(WorkspaceId::new());
    assert!(matches!(
        host.call(&s, Tool::Tap { x: 1.0, y: 1.0 }),
        Err(ToolError::Failed(m)) if m.contains("shutting down")
    ));
}

#[test]
fn screenshot_encodes_the_frame_with_point_size() {
    let (host, sessions, rx) = host(short());
    fake_consumer(rx, Log::default());
    let s = sessions.register(WorkspaceId::new());
    let out = host.call(&s, Tool::Screenshot).unwrap();
    let Some(Content::Png(png)) = out.content.first() else {
        panic!("expected an image first: {out:?}");
    };
    let img = image::load_from_memory_with_format(png, image::ImageFormat::Png).unwrap();
    assert_eq!((img.width(), img.height()), (471, 1024));
    assert!(text(&out).contains("402 x 874 points"), "{}", text(&out));
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
fn orchestration_tools_map_to_requests() {
    let (host, sessions, rx, on) = orchestration_host(short());
    on.store(true, Ordering::Relaxed);
    let log = Log::default();
    fake_consumer(rx, log.clone());
    let ws = WorkspaceId::new();
    let s = sessions.register(ws);

    let out = host.call(&s, Tool::WorkspacesList).unwrap();
    let listed: serde_json::Value = serde_json::from_str(&text(&out)).unwrap();
    assert_eq!(listed["workspaces"][0]["name"], "Login");
    assert_eq!(listed["workspaces"][0]["state"], "Working");

    let out = host
        .call(
            &s,
            Tool::WorkspaceCreate {
                task: "Write docs".into(),
                name: None,
                agent: Some("claude".into()),
            },
        )
        .unwrap();
    let created: serde_json::Value = serde_json::from_str(&text(&out)).unwrap();
    assert_eq!(created["id"], "c2");
    assert_eq!(created["agent_start"], "queued");
    assert_eq!(created["queued_behind"], 1);

    let out = host
        .call(
            &s,
            Tool::AgentPrompt {
                workspace: "Login".into(),
                text: "Add a test".into(),
            },
        )
        .unwrap();
    assert_eq!(text(&out), "Sent to the agent of “Login”.");
    assert_eq!(
        host.call(
            &s,
            Tool::AgentPrompt {
                workspace: "Other".into(),
                text: "hi".into(),
            },
        ),
        Err(ToolError::Failed("no workspace \"Other\"".into()))
    );

    let out = host
        .call(
            &s,
            Tool::AgentTranscript {
                workspace: "Login".into(),
                last: 3,
            },
        )
        .unwrap();
    assert_eq!(text(&out), "[user]\nFix login\n\n[agent]\nDone.");
    assert_eq!(
        host.call(
            &s,
            Tool::AgentTranscript {
                workspace: "Shell".into(),
                last: 10,
            },
        ),
        Err(ToolError::Unsupported(
            "transcripts of terminal agents".into()
        ))
    );

    let log = log.lock().unwrap();
    assert_eq!(
        *log,
        vec![
            format!("{ws} children"),
            format!("{ws} create Write docs None Some(\"claude\")"),
            format!("{ws} prompt Login Add a test"),
            format!("{ws} prompt Other hi"),
            format!("{ws} transcript Login 3"),
            format!("{ws} transcript Shell 10"),
        ]
    );
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
