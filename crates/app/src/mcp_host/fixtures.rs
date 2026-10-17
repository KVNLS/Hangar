//! Test doubles shared by the host's test modules: hosts on a channel and a device that answers.

use super::*;
use hangar_core::device::Platform;
use hangar_mcp::{Content, Sharing};
use std::sync::Arc;

pub(super) type Log = Arc<Mutex<Vec<String>>>;

pub(super) fn host(timeouts: Timeouts) -> (McpHost, Sessions, mpsc::UnboundedReceiver<McpRequest>) {
    let (host, sessions, rx, _) = orchestration_host(timeouts);
    (host, sessions, rx)
}

/// As [`host`], with the switch of the orchestration tools (off).
pub(super) fn orchestration_host(
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
        McpHost::new(
            sessions.clone(),
            tx,
            timeouts,
            Arc::clone(&on),
            Notices::default(),
        )
        .unwrap(),
        sessions,
        rx,
        on,
    )
}

pub(super) fn short() -> Timeouts {
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
pub(super) fn fake_consumer(mut rx: mpsc::UnboundedReceiver<McpRequest>, log: Log) {
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
                        sharing: None,
                        platforms: Vec::new(),
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
                        Input::Back => Err(anyhow::Error::new(
                            crate::adapters::devices::Error::Unsupported("Back on iOS"),
                        )
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
                McpRequest::Text { text, reply, .. } if text == "queued" => {
                    record("text queued".into());
                    // The app side gives up waiting in line after its budget.
                    std::thread::spawn(move || {
                        std::thread::sleep(short().acquire_budget());
                        let message = hangar_mcp::queued_message(&shared(false));
                        let _ = reply.send(Err(anyhow::Error::new(ToolError::Failed(message))));
                    });
                }
                McpRequest::Text { text, reply, .. } => {
                    record(format!("text {text}"));
                    let _ = reply.send(Ok(()));
                }
                McpRequest::Acquire { wait, reply, .. } => {
                    record(format!("acquire {wait:?}"));
                    let granted = !wait.is_zero();
                    let _ = reply.send(Ok(Acquisition {
                        granted,
                        sharing: shared(granted),
                    }));
                }
                McpRequest::Release { reply, .. } => {
                    record("release".into());
                    let _ = reply.send(Ok(Released {
                        had_waiters: true,
                        next: Some("ws-b".into()),
                        left_line: false,
                    }));
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

/// The fake device "iPhone" shared by ws-a and ws-b; `held` by the caller, else ws-a holds it
/// and the caller is next.
pub(super) fn shared(held: bool) -> Sharing {
    Sharing {
        device_name: "iPhone".into(),
        device_id: Some("UDID".into()),
        holder: Some(if held { "ws-b" } else { "ws-a" }.into()),
        you_hold: held,
        position: (!held).then_some(0),
        queue: if held { vec![] } else { vec!["ws-b".into()] },
        shared_with: vec!["ws-a".into()],
        reservation_left_s: Some(60),
        estimated_wait_s: (!held).then_some(60),
        idle_release_s: 90,
        stale_app_from: None,
        notice: None,
        switched_from: None,
    }
}

pub(super) fn text(out: &ToolOutput) -> String {
    out.content
        .iter()
        .filter_map(|c| match c {
            Content::Text(t) => Some(t.clone()),
            Content::Png(_) => None,
        })
        .collect()
}
