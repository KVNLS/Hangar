use hangar_core::device::Platform;
use serde_json::{Value, json};

use super::*;
use crate::tools::{Content, DeviceStatus, Tool};

fn sharing() -> Sharing {
    Sharing {
        device_name: "iPhone 17".into(),
        device_id: Some("UDID".into()),
        holder: Some("ws-b".into()),
        position: Some(1),
        queue: vec!["ws-c".into(), "ws-a".into()],
        shared_with: vec!["ws-b".into(), "ws-c".into()],
        estimated_wait_s: Some(200),
        idle_release_s: 90,
        ..Sharing::default()
    }
}

fn text(out: &ToolOutput) -> String {
    match &out.content[0] {
        Content::Text(t) => t.clone(),
        Content::Png(_) => unreachable!(),
    }
}

fn json_of(out: ToolOutput) -> Value {
    serde_json::from_str(&text(&out)).unwrap()
}

#[test]
fn queued_message_cases() {
    assert_eq!(
        queued_message(&sharing()),
        "iPhone 17 (shared with ws-b, ws-c) is in use by ws-b; you are 2nd in line, about 4 min. \
         Your place is kept: call device_acquire with wait_s to wait, or device_info to check."
    );
    let s = Sharing {
        holder: None,
        position: Some(0),
        estimated_wait_s: None,
        shared_with: vec![],
        ..sharing()
    };
    assert_eq!(
        queued_message(&s),
        "iPhone 17 is not available yet (starting up); you are 1st in line, wait unknown. \
         Your place is kept: call device_acquire with wait_s to wait, or device_info to check."
    );
    let s = Sharing {
        position: Some(10),
        estimated_wait_s: Some(30),
        ..sharing()
    };
    assert!(queued_message(&s).contains("11th in line, about 30 s."));
    let s = Sharing {
        position: Some(2),
        ..sharing()
    };
    assert!(queued_message(&s).contains("3rd in line"));
    let s = Sharing {
        position: None,
        ..sharing()
    };
    assert!(queued_message(&s).contains("; you are waiting, about 4 min."));
}

#[test]
fn ordinals() {
    let all: Vec<String> = [1, 2, 3, 4, 11, 12, 13, 21, 22, 101, 112]
        .map(ordinal)
        .into();
    assert_eq!(
        all,
        [
            "1st", "2nd", "3rd", "4th", "11th", "12th", "13th", "21st", "22nd", "101st", "112th"
        ]
    );
}

#[test]
fn acquired_json_shape() {
    let v = json_of(ToolOutput::device_acquired(false, &sharing()));
    assert_eq!(v["status"], "queued");
    assert_eq!(v["device"], json!({"name": "iPhone 17", "id": "UDID"}));
    assert_eq!(v["holder"], "ws-b");
    assert_eq!(v["position"], 1);
    assert_eq!(v["queue"], json!(["ws-c", "ws-a"]));
    assert_eq!(v["estimated_wait_s"], 200);
    assert_eq!(v["idle_release_s"], 90);
    assert_eq!(v["reservation_left_s"], Value::Null);
    assert_eq!(v["stale_app_from"], Value::Null);
    assert_eq!(v["notice"], Value::Null);
    let granted = json_of(ToolOutput::device_acquired(true, &sharing()));
    assert_eq!(granted["status"], "granted");
}

#[test]
fn released_text() {
    assert!(text(&ToolOutput::device_released(true, Some("ws-c"), false)).contains("ws-c is next"));
    assert!(text(&ToolOutput::device_released(true, None, false)).contains("next workspace"));
    assert!(text(&ToolOutput::device_released(false, None, false)).contains("nobody"));
    assert!(text(&ToolOutput::device_released(false, None, true)).contains("left the line"));
}

#[test]
fn device_status_carries_sharing() {
    let mut status = DeviceStatus {
        platform: Platform::Ios,
        name: "iPhone 17".into(),
        id: "UDID".into(),
        booted: true,
        screen_pt: None,
        scale: None,
        orientation: None,
        sharing: None,
    };
    assert_eq!(
        json_of(ToolOutput::device_status(&status))["sharing"],
        Value::Null
    );
    status.sharing = Some(Sharing {
        you_hold: true,
        stale_app_from: Some("ws-b".into()),
        notice: Some("ws-c is waiting; call device_release when done".into()),
        ..sharing()
    });
    let v = json_of(ToolOutput::device_status(&status));
    assert_eq!(v["sharing"]["you_hold"], true);
    assert_eq!(v["sharing"]["stale_app_from"], "ws-b");
    assert_eq!(v["sharing"]["holder"], "ws-b");
}

#[test]
fn parses_acquire_and_release() {
    let p = |args: Value| Tool::parse("device_acquire", Some(&args));
    assert_eq!(p(json!({})), Ok(Tool::DeviceAcquire { wait_s: 0 }));
    assert_eq!(
        p(json!({"wait_s": 25})),
        Ok(Tool::DeviceAcquire { wait_s: 25 })
    );
    assert!(p(json!({"wait_s": 26})).is_err());
    assert!(p(json!({"wait_s": -1})).is_err());
    assert!(p(json!({"wait_s": "5"})).is_err());
    assert!(p(json!({"x": 1})).is_err());
    assert_eq!(Tool::parse("device_release", None), Ok(Tool::DeviceRelease));
    assert!(Tool::parse("device_release", Some(&json!({"a": 1}))).is_err());
    assert!(!Tool::is_orchestration_name("device_acquire"));
    assert!(!Tool::is_orchestration_name("device_release"));
}
