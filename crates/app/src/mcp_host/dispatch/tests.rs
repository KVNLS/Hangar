//! Tool calls mapped to requests and the answers to results.

use super::*;
use crate::mcp_host::Timeouts;
use crate::mcp_host::fixtures::{
    Log, fake_consumer, host, orchestration_host, shared, short, text,
};
use hangar_core::device::LogLevel;
use hangar_mcp::{Content, ToolHost};
use std::sync::atomic::Ordering;
use std::time::Instant;

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
fn device_acquire_and_release_round_trip() {
    let (host, sessions, rx) = host(short());
    let log = Log::default();
    fake_consumer(rx, log.clone());
    let ws = WorkspaceId::new();
    let s = sessions.register(ws);

    let out = host
        .call(
            &s,
            Tool::DeviceAcquire {
                wait_s: 0,
                platform: None,
            },
        )
        .unwrap();
    let queued: serde_json::Value = serde_json::from_str(&text(&out)).unwrap();
    assert_eq!(queued["status"], "queued");
    assert_eq!(queued["position"], 0);
    assert_eq!(queued["holder"], "ws-a");

    // The wait is passed on; the request may take that long on top of a call.
    let out = host
        .call(
            &s,
            Tool::DeviceAcquire {
                wait_s: 3,
                platform: None,
            },
        )
        .unwrap();
    let granted: serde_json::Value = serde_json::from_str(&text(&out)).unwrap();
    assert_eq!(granted["status"], "granted");
    assert_eq!(granted["idle_release_s"], 90);
    assert_eq!(granted["notice"], serde_json::Value::Null);

    let out = host.call(&s, Tool::DeviceRelease).unwrap();
    assert_eq!(text(&out), "Device released; ws-b is next in line.");
    assert_eq!(
        *log.lock().unwrap(),
        vec![
            format!("{ws} acquire 0ns"),
            format!("{ws} acquire 3s"),
            format!("{ws} release"),
        ]
    );
}

#[test]
fn a_device_tool_queued_past_its_budget_reports_its_place() {
    assert_eq!(
        Timeouts::default().acquire_budget(),
        Duration::from_secs(27)
    );
    assert!(short().acquire_budget() < short().call);
    let (host, sessions, rx) = host(short());
    fake_consumer(rx, Log::default());
    let s = sessions.register(WorkspaceId::new());
    let err = host.call(
        &s,
        Tool::TypeText {
            text: "queued".into(),
        },
    );
    let Err(ToolError::Failed(message)) = err else {
        panic!("a queued call fails: {err:?}");
    };
    assert_eq!(message, hangar_mcp::queued_message(&shared(false)));
    assert!(message.contains("1st in line"), "{message}");
    assert!(!message.contains("timed out"));
}

#[test]
fn pending_notices_ride_on_the_next_device_result() {
    let (host, sessions, rx) = host(short());
    fake_consumer(rx, Log::default());
    let (ws, other) = (WorkspaceId::new(), WorkspaceId::new());
    let s = sessions.register(ws);
    let o = sessions.register(other);

    host.notices.push(ws, "ws-c is waiting for iPhone".into());
    let out = host.call(&o, Tool::Tap { x: 1.0, y: 1.0 }).unwrap();
    assert!(!text(&out).contains("Notice"), "another workspace's notice");
    host.call(&s, Tool::WorkspacesList).unwrap_err();
    let out = host.call(&s, Tool::Tap { x: 1.0, y: 1.0 }).unwrap();
    assert_eq!(
        text(&out),
        "Tapped at (1, 1).Notice: ws-c is waiting for iPhone"
    );
    let out = host.call(&s, Tool::Tap { x: 1.0, y: 1.0 }).unwrap();
    assert_eq!(text(&out), "Tapped at (1, 1).", "delivered once");

    host.notices.push(ws, "revoked".into());
    let out = host
        .call(
            &s,
            Tool::DeviceAcquire {
                wait_s: 1,
                platform: None,
            },
        )
        .unwrap();
    let got: serde_json::Value = serde_json::from_str(&text(&out)).unwrap();
    assert_eq!(got["notice"], "revoked", "in the sharing state");
    assert_eq!(out.content.len(), 1);

    host.notices.push(ws, "stale".into());
    let err = host.call(
        &s,
        Tool::Press(hangar_mcp::Press::Button(hangar_mcp::Button::Back)),
    );
    assert_eq!(
        err,
        Err(ToolError::Unsupported(
            "Back on iOS\n\nNotice: stale".into()
        ))
    );
}
