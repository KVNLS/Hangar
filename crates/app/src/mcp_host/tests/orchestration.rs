//! Orchestration tools: off by default, mapped to requests, and the transcripts they may read.

use super::*;

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
