use super::*;

fn sh(script: &str) -> AcpAdapter {
    let mut a = AcpAdapter::new("t", "t", "/bin/sh");
    a.args = vec!["-c".into(), script.into()];
    a
}

#[test]
fn resume_loads_or_falls_back() {
    let dir = tempfile::tempdir().expect("tempdir");
    // Loads: initialize (1), session/load (2) with one replayed update before the answer.
    let script = r#"
        read l; echo '{"jsonrpc":"2.0","id":1,"result":{"protocolVersion":1,"agentCapabilities":{"loadSession":true}}}'
        read l; case "$l" in *session/load*) ;; *) exit 9;; esac
        echo '{"jsonrpc":"2.0","method":"session/update","params":{"sessionId":"old","update":{"sessionUpdate":"user_message_chunk","content":{"type":"text","text":"hi"}}}}'
        echo '{"jsonrpc":"2.0","id":2,"result":{}}'
        read l
    "#;
    let (session, events) = sh(script)
        .start(&SpawnContext::new(dir.path()), &[], Some("old"))
        .expect("resumes");
    assert!(session.resumed());
    assert_eq!(session.session_id(), "old");
    assert!(matches!(
        events.recv_timeout(Duration::from_secs(5)),
        Ok(AcpEvent::Update(protocol::SessionUpdate::UserMessage(_)))
    ));
    session.kill();

    // Load refused: a new session is created and the reason kept.
    let script = r#"
        read l; echo '{"jsonrpc":"2.0","id":1,"result":{"protocolVersion":1,"agentCapabilities":{"loadSession":true}}}'
        read l; echo '{"jsonrpc":"2.0","id":2,"error":{"code":-32002,"message":"Session not found"}}'
        read l; echo '{"jsonrpc":"2.0","id":3,"result":{"sessionId":"new"}}'
        read l
    "#;
    let (session, _events) = sh(script)
        .start(&SpawnContext::new(dir.path()), &[], Some("gone"))
        .expect("falls back");
    assert!(!session.resumed());
    assert_eq!(session.session_id(), "new");
    assert!(
        session
            .resume_error()
            .is_some_and(|e| e.contains("Session not found"))
    );
    session.kill();
}

#[test]
fn saved_config_applies_model_first_and_skips_unknown_values() {
    let dir = tempfile::tempdir().expect("tempdir");
    // session/new offers a model without efforts; choosing `big` adds the effort option.
    let script = r#"
        read l; echo '{"jsonrpc":"2.0","id":1,"result":{"protocolVersion":1}}'
        read l; echo '{"jsonrpc":"2.0","id":2,"result":{"sessionId":"s","configOptions":[{"id":"model","name":"Model","category":"model","type":"select","currentValue":"small","options":[{"value":"small","name":"Small"},{"value":"big","name":"Big"}]}]}}'
        read l; case "$l" in *'"configId":"model"'*'"value":"big"'*) ;; *) exit 9;; esac
        echo '{"jsonrpc":"2.0","id":3,"result":{"configOptions":[{"id":"model","name":"Model","category":"model","type":"select","currentValue":"big","options":[{"value":"small","name":"Small"},{"value":"big","name":"Big"}]},{"id":"effort","name":"Effort","category":"thought_level","type":"select","currentValue":"medium","options":[{"value":"medium","name":"Medium"},{"value":"high","name":"High"}]}]}}'
        read l; case "$l" in *'"configId":"effort"'*'"value":"high"'*) ;; *) exit 9;; esac
        echo '{"jsonrpc":"2.0","id":4,"result":{"configOptions":[{"id":"model","name":"Model","category":"model","type":"select","currentValue":"big","options":[{"value":"big","name":"Big"}]},{"id":"effort","name":"Effort","category":"thought_level","type":"select","currentValue":"high","options":[{"value":"medium","name":"Medium"},{"value":"high","name":"High"}]}]}}'
        read l
    "#;
    let (mut session, _events) = sh(script)
        .start(&SpawnContext::new(dir.path()), &[], None)
        .expect("starts");
    assert_eq!(session.config_options().len(), 1);
    let wanted = [
        ("effort".to_owned(), "high".to_owned()),
        ("fast".to_owned(), "on".to_owned()),
        ("model".to_owned(), "big".to_owned()),
    ];
    assert!(session.apply_config(&wanted).is_empty());
    let current: Vec<(&str, &str)> = session
        .config_options()
        .iter()
        .map(|o| (o.id.as_str(), o.current.as_str()))
        .collect();
    assert_eq!(current, [("model", "big"), ("effort", "high")]);
    session.kill();
}

#[test]
fn watchdog_stops_the_agent_group_when_the_owner_dies() {
    // A stand-in for Hangar that dies after a second, and an "agent" in its own group that
    // would run for a minute.
    let mut owner = Command::new("/bin/sleep").arg("1").spawn().expect("owner");
    let mut agent = Command::new("/bin/sleep")
        .arg("60")
        .process_group(0)
        .spawn()
        .expect("agent");
    spawn_watchdog(owner.id(), agent.id());
    owner.wait().expect("owner exits");
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    loop {
        if agent.try_wait().expect("try_wait").is_some() {
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "the agent outlived its owner"
        );
        std::thread::sleep(Duration::from_millis(100));
    }
}

#[test]
fn watchdog_leaves_when_the_agent_exits() {
    let mut agent = Command::new("/bin/sleep")
        .arg("1")
        .process_group(0)
        .spawn()
        .expect("agent");
    let group = agent.id();
    spawn_watchdog(std::process::id(), group);
    agent.wait().expect("agent exits");
    let deadline = std::time::Instant::now() + Duration::from_secs(8);
    // SAFETY: signal 0 only checks whether the group still has a process.
    while unsafe { libc::killpg(group as libc::pid_t, 0) } == 0 {
        assert!(std::time::Instant::now() < deadline, "the watchdog lingers");
        std::thread::sleep(Duration::from_millis(100));
    }
}

#[test]
fn auth_required_names_the_login_step() {
    let dir = tempfile::tempdir().expect("tempdir");
    let script = r#"
        read l; echo '{"jsonrpc":"2.0","id":1,"result":{"protocolVersion":1,"authMethods":[{"id":"oauth","name":"Log in"}]}}'
        read l; echo '{"jsonrpc":"2.0","id":2,"error":{"code":-32000,"message":"API key is missing"}}'
        read l
    "#;
    let err = sh(script)
        .with_login_hint("Run `t login`.")
        .start(&SpawnContext::new(dir.path()), &[], None)
        .expect_err("needs a login");
    assert_eq!(
        err.to_string(),
        "t is not signed in (API key is missing). Run `t login`."
    );
}

#[test]
fn exit_during_handshake_reports_stderr() {
    let dir = tempfile::tempdir().expect("tempdir");
    let err = sh("echo 'node: not found' >&2; exit 3")
        .start(&SpawnContext::new(dir.path()), &[], None)
        .expect_err("fails");
    match err {
        Error::AgentExited { stderr_tail, .. } => {
            assert!(stderr_tail.contains("node: not found"), "{stderr_tail}");
        }
        other => panic!("unexpected {other:?}"),
    }
}

#[test]
fn scripted_agent_handshake_and_turn() {
    let dir = tempfile::tempdir().expect("tempdir");
    // Answers initialize, session/new and one prompt by request order (ids 1, 2, 3).
    let script = r#"
        read l; echo '{"jsonrpc":"2.0","id":1,"result":{"protocolVersion":1,"agentCapabilities":{"loadSession":true}}}'
        read l; echo '{"jsonrpc":"2.0","id":2,"result":{"sessionId":"s-1"}}'
        read l; echo '{"jsonrpc":"2.0","method":"session/update","params":{"sessionId":"s-1","update":{"sessionUpdate":"agent_message_chunk","content":{"type":"text","text":"done"}}}}'
        echo '{"jsonrpc":"2.0","id":3,"result":{"stopReason":"end_turn"}}'
        read l
    "#;
    let (session, events) = sh(script)
        .start(&SpawnContext::new(dir.path()), &[], None)
        .expect("starts");
    assert_eq!(session.session_id(), "s-1");
    assert!(session.info.load_session);
    let rx = &events;
    session.prompt("go").expect("prompt");
    let wait = |rx: &Receiver<AcpEvent>| rx.recv_timeout(Duration::from_secs(5)).expect("event");
    assert!(matches!(wait(rx), AcpEvent::Update(_)));
    assert_eq!(
        wait(rx),
        AcpEvent::TurnEnded {
            reason: protocol::StopReason::EndTurn,
            usage: None
        }
    );
    session.kill();
    assert!(matches!(wait(rx), AcpEvent::Exited { .. }));
    assert!(session.wait_for_exit(Duration::from_secs(1)));
    assert!(matches!(session.set_mode("plan"), Err(Error::Protocol(_))));
}

#[test]
fn applied_mode_becomes_the_initial_mode() {
    let dir = tempfile::tempdir().expect("tempdir");
    let script = r#"
        read l; echo '{"jsonrpc":"2.0","id":1,"result":{"protocolVersion":1}}'
        read l; echo '{"jsonrpc":"2.0","id":2,"result":{"sessionId":"s-1","modes":{"currentModeId":"default","availableModes":[{"id":"default","name":"Manual"},{"id":"bypassPermissions","name":"Bypass"}]},"configOptions":[{"id":"mode","category":"mode","type":"select","currentValue":"default","options":[{"value":"default","name":"Manual"},{"value":"bypassPermissions","name":"Bypass"}]}]}}'
        read l; case "$l" in *session/set_mode*bypassPermissions*) echo '{"jsonrpc":"2.0","id":3,"result":{}}';; esac
        read l
    "#;
    let (mut session, _events) = sh(script)
        .start(&SpawnContext::new(dir.path()), &[], None)
        .expect("starts");
    session.apply_mode("default").expect("already in it");
    session.apply_mode("bypassPermissions").expect("switches");
    assert_eq!(session.initial_mode(), Some("bypassPermissions"));
    assert_eq!(session.config_options()[0].current, "bypassPermissions");
    session.kill();
}
