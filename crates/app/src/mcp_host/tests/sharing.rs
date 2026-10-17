//! Device sharing: acquire and release, a call queued past its budget, pending notices.

use super::*;

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
