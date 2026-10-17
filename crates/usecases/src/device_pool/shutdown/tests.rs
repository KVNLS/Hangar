//! Shutting devices down: the cap, memory pressure, archived and deleted workspaces, the
//! launch scan.

use hangar_core::device::{Running, RunningInstance, RunningSet};

use crate::device_pool::fixtures::*;
use crate::device_pool::*;

#[test]
fn archiving_cancels_the_workspace_waiters() {
    let (a, b) = (workspace("a"), workspace("b"));
    let mut m = pool(Limit::Manual(1), &[(&a, ios("X")), (&b, ios("X"))]);
    let now = Instant::now();
    let x = base_key("X");
    m.request_lease(&x, a.id, now).unwrap();
    let Ok(LeaseStart::Queued(token)) = m.request_lease(&x, b.id, now) else {
        panic!("b should wait");
    };
    assert!(m.archive(b.id, now));
    assert!(matches!(
        m.take_commands().as_slice(),
        [Command::FailLease { token: t, message }] if *t == token && message == "the workspace was closed"
    ));
}

#[test]
fn critical_pressure_shuts_the_idle_device_down() {
    let (a, b) = (workspace("a"), workspace("b"));
    let mut m = pool(Limit::Auto, &[(&a, ios("X")), (&b, ios("Y"))]);
    let now = Instant::now();
    let (ka, kb) = (
        PoolKey::Own(a.id, Platform::Ios),
        PoolKey::Own(b.id, Platform::Ios),
    );
    for (key, ws) in [(&ka, &a), (&kb, &b)] {
        let udid = ws.slug.as_str();
        wait(m.open(key, now));
        let probe = step(&m.take_commands());
        let running = Running {
            info: info(udid, udid),
            created: None,
        };
        m.step_done(key, probe.attempt, StepDone::Probed(Ok(Some(running))), now);
        let open = step(&m.take_commands());
        m.step_done(
            key,
            open.attempt,
            StepDone::Opened(Ok((Sess(info(udid, udid)), None))),
            now,
        );
        m.take_commands();
    }
    m.set_focused(Some(a.id));
    m.monitor(now, Ok(Pressure::Critical), None);
    let commands = m.take_commands();
    assert!(matches!(
        commands.as_slice(),
        [Command::Notice(NoticeLevel::Warning, n), Command::Shutdown(job)]
            if n.starts_with("Memory pressure is critical: shut down b") && job.key == kb
    ));
    assert_eq!(m.pressure(), Some(Pressure::Critical));
    // A simulator shut down outside Hangar is closed, with a word to the user.
    assert_eq!(m.open_simulators(), ["a"]);
    m.monitor(now, Ok(Pressure::Normal), Some(Ok(Vec::new())));
    assert!(matches!(
        m.take_commands().as_slice(),
        [Command::Notice(NoticeLevel::Warning, n)] if n.starts_with("a was shut down outside Hangar")
    ));
    assert!(matches!(
        m.status(a.id, now),
        DeviceStatus::Closed {
            error: Some("shut down outside Hangar"),
            ..
        }
    ));
}

#[test]
fn launch_scan_adopts_devices_left_running() {
    let (a, b) = (workspace("a"), workspace("b"));
    let mut left = ios("B");
    left.own = Some("LEFT".into());
    let mut m = pool(Limit::Auto, &[]);
    m.track(a.clone());
    m.track(b.clone());
    m.resolved(a.id, left);
    m.resolved(b.id, ios("B"));
    let mut running = RunningSet::default();
    running.add(
        Platform::Ios,
        vec![RunningInstance {
            id: "LEFT".into(),
            base: None,
        }],
    );
    m.running_scanned(&running);
    assert_eq!(m.booted_count(), 1);
    let rows = m.rows();
    assert_eq!(rows.len(), 1, "b has nothing running: no row");
    assert_eq!((rows[0].state, rows[0].busy), ("running", false));
    // Running but not open: shutting down needs its id only.
    let job = m.shutdown(&PoolKey::Own(a.id, Platform::Ios)).unwrap();
    assert!(matches!(job.kind, ShutdownKind::Device { ref id, .. } if id == "LEFT"));
}

#[test]
fn lowering_the_limit_shuts_idle_extras_down() {
    let (a, b) = (workspace("a"), workspace("b"));
    let mut m = pool(Limit::Auto, &[(&a, ios("X")), (&b, ios("Y"))]);
    let now = Instant::now();
    for ws in [&a, &b] {
        let key = PoolKey::Own(ws.id, Platform::Ios);
        wait(m.open(&key, now));
        let probe = step(&m.take_commands());
        let r = Running {
            info: info(&ws.slug, &ws.slug),
            created: None,
        };
        m.step_done(&key, probe.attempt, StepDone::Probed(Ok(Some(r))), now);
        let open = step(&m.take_commands());
        m.step_done(
            &key,
            open.attempt,
            StepDone::Opened(Ok((Sess(info(&ws.slug, &ws.slug)), None))),
            now,
        );
        m.take_commands();
    }
    m.set_limit(Limit::Manual(1), Instant::now());
    let commands = m.take_commands();
    let shutdowns: Vec<&PoolKey> = commands
        .iter()
        .filter_map(|c| match c {
            Command::Shutdown(job) => Some(&job.key),
            _ => None,
        })
        .collect();
    assert_eq!(
        shutdowns,
        [&PoolKey::Own(a.id, Platform::Ios)],
        "least recently used first"
    );
    assert!(
        commands
            .iter()
            .any(|c| matches!(c, Command::Save(Save::Limit(Limit::Manual(1)))))
    );
}

#[test]
fn deleting_a_clone_needs_one() {
    let (a, b) = (workspace("a"), workspace("b"));
    let mut own = ios("B");
    own.own = Some("CLONE".into());
    let mut m = pool(Limit::Auto, &[(&a, own), (&b, ios("B"))]);
    assert!(m.delete_clone(&base_key("B")).is_err());
    assert_eq!(
        m.delete_clone(&PoolKey::Own(b.id, Platform::Ios))
            .err()
            .as_deref(),
        Some("no simulator was created for this workspace")
    );
    let (udid, shutdown) = m.delete_clone(&PoolKey::Own(a.id, Platform::Ios)).unwrap();
    assert_eq!((udid.as_str(), shutdown.is_none()), ("CLONE", true));
    assert_eq!(m.assignment(a.id).and_then(|a| a.own.clone()), None);
    assert_eq!(m.forget(a.id).active.map(|a| a.base), Some("B".into()));
    assert!(m.assignment(a.id).is_none());
}

#[test]
fn archiving_drops_the_binding_and_fails_each_ticket_once() {
    let ws = workspaces(&["w1", "w2", "w3", "w4"]);
    let assigned: Vec<(&WorkspaceRef, Assignment)> = ws.iter().map(|w| (w, ios("B"))).collect();
    let mut m = pool(Limit::Manual(2), &assigned);
    let t = Instant::now();
    let k1 = PoolKey::Own(ws[0].id, Platform::Ios);
    m.request_device(ws[0].id, t).unwrap();
    m.request_device(ws[1].id, t).unwrap();
    let w3: Vec<Token> = (0..2)
        .map(|_| queued(m.request_device(ws[2].id, t).map(|s| s.lease)))
        .collect();
    let w4 = queued(m.request_device(ws[3].id, t).map(|s| s.lease));
    assert_eq!(m.key_for(ws[2].id), Some(k1.clone()));
    assert_eq!(
        m.key_for(ws[3].id),
        Some(PoolKey::Own(ws[1].id, Platform::Ios)),
        "shortest queue"
    );
    m.take_commands();
    assert!(m.archive(ws[2].id, t));
    assert_eq!(failed(&m.take_commands()), w3);
    assert_eq!(
        m.key_for(ws[2].id),
        Some(PoolKey::Own(ws[2].id, Platform::Ios))
    );
    // The owner is archived: who shares its device loses it.
    assert!(m.archive(ws[1].id, t));
    let commands = m.take_commands();
    assert_eq!(failed(&commands), [w4]);
    assert!(commands.iter().any(|c| matches!(
        c,
        Command::FailLease { message, .. } if message == "Hangar · app/w2 was shut down with its workspace"
    )));
    assert_eq!(
        m.key_for(ws[3].id),
        Some(PoolKey::Own(ws[3].id, Platform::Ios))
    );
    assert!(grants(&commands).is_empty());
}

#[test]
fn archive_and_forget_cover_both_own_devices() {
    let (a, w) = (workspace("a"), workspace("w"));
    let mut m = pool(Limit::Auto, &[(&a, ios_own("B", "CLONE")), (&w, ios("B"))]);
    let t = Instant::now();
    let (ios_key, android_key) = (own(&a, Platform::Ios), own(&a, Platform::Android));
    m.request_device(a.id, t).unwrap();
    open_running(&mut m, &ios_key, "CLONE", t);
    m.release_lease(&ios_key, a.id, t);
    done(m.switch_platform(a.id, android("P"), SwitchBy::Agent, t));
    // W shares the parked device, which runs.
    m.shutdown_finished(&ios_key, Err("busy".into()), t);
    let for_w = m.request_lease(&ios_key, w.id, t).unwrap();
    assert_eq!(for_w, LeaseStart::Granted);
    open_running(&mut m, &android_key, "emulator-5554", t);
    m.take_commands();
    assert!(m.archive(a.id, t));
    let commands = m.take_commands();
    assert!(
        commands.iter().any(|c| matches!(
            c,
            Command::Revoked { holder, key } if *holder == w.id && *key == ios_key
        )),
        "{commands:?}"
    );
    let shutdowns = m.shutdown_own(a.id);
    let keys: Vec<&PoolKey> = shutdowns
        .iter()
        .map(|s| match s {
            OwnShutdown::Job(job) => &job.key,
            other => panic!("{other:?}"),
        })
        .collect();
    assert_eq!(keys, [&android_key, &ios_key]);
    let forgotten = m.forget(a.id);
    assert_eq!(
        forgotten.active.map(|a| (a.platform, a.serial)),
        Some((Platform::Android, Some("emulator-5554".into())))
    );
    assert_eq!(forgotten.parked, Some(ios_own("B", "CLONE")));
    assert!(
        m.rows()
            .iter()
            .all(|r| r.key != ios_key && r.key != android_key)
    );
}

#[test]
fn switching_back_while_the_device_shuts_down_reopens_it_after() {
    let a = workspace("a");
    let mut m = pool(Limit::Auto, &[(&a, ios_own("B", "CLONE"))]);
    let t = Instant::now();
    let key = own(&a, Platform::Ios);
    open_running(&mut m, &key, "CLONE", t);
    let (shutdown, _) = done(m.switch_platform(a.id, android("P"), SwitchBy::Agent, t));
    assert_eq!(shutdown.map(|job| job.key), Some(key.clone()));
    done(m.switch_platform(a.id, ios("B"), SwitchBy::Agent, t));
    m.take_commands();
    // A call is granted the device, and its open waits for the shutdown.
    let start = m.request_device(a.id, t).unwrap();
    assert_eq!((&start.key, start.lease), (&key, LeaseStart::Granted));
    let token = wait(m.open_held(&key, a.id, t));
    assert!(m.take_commands().is_empty());
    m.shutdown_finished(&key, Ok(()), t + secs(1));
    let probe = step(&m.take_commands());
    assert_eq!(probe.attempt, token);
    let clone = Target::IosOwn {
        udid: Some("CLONE".into()),
        base: "B".into(),
        name: "Hangar · app/a".into(),
    };
    assert_eq!(probe.step, Step::Probe(clone));
    let running = Running {
        info: info("CLONE", "CLONE"),
        created: None,
    };
    m.step_done(&key, probe.attempt, StepDone::Probed(Ok(Some(running))), t);
    let open = step(&m.take_commands());
    m.step_done(
        &key,
        open.attempt,
        StepDone::Opened(Ok((Sess(info("CLONE", "CLONE")), None))),
        t,
    );
    let commands = m.take_commands();
    assert!(matches!(
        replies(&commands).as_slice(),
        [(t, Ok(_))] if *t == token
    ));
    assert!(!shut_down(&commands, &key));
}

#[test]
fn a_device_removed_while_shutting_down_answers_its_reopen() {
    let a = workspace("a");
    let mut m = pool(Limit::Auto, &[(&a, ios("B"))]);
    let t = Instant::now();
    let key = own(&a, Platform::Ios);
    open_running(&mut m, &key, "DA", t);
    assert!(m.shutdown(&key).is_some());
    let token = wait(m.open(&key, t));
    m.forget(a.id);
    assert_eq!(
        replies(&m.take_commands()),
        [(token, Err(OpenError::Failed(STOPPED.into())))]
    );
}
