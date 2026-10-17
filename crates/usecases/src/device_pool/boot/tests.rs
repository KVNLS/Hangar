//! Bringing a device up: probe, claim a slot, check the host, create, boot, open.

use hangar_core::device::{Running, RunningInstance, RunningSet};

use crate::device_pool::fixtures::*;
use crate::device_pool::*;

#[test]
fn own_simulator_is_created_booted_and_opened() {
    let w = workspace("login");
    let mut m = pool(Limit::Auto, &[(&w, ios("BASE"))]);
    let now = Instant::now();
    let key = m.key_for(w.id).unwrap();
    assert_eq!(key, PoolKey::Own(w.id, Platform::Ios));
    let token = wait(m.open(&key, now));
    let probe = step(&m.take_commands());
    let own = Target::IosOwn {
        udid: None,
        base: "BASE".into(),
        name: "Hangar · app/login".into(),
    };
    assert_eq!(probe.step, Step::Probe(own.clone()));
    assert!(matches!(
        m.status(w.id, now),
        DeviceStatus::Opening { status, .. } if status == "checking the device… 0 s"
    ));
    let attempt = probe.attempt;
    m.step_done(&key, attempt, StepDone::Probed(Ok(None)), now);
    assert_eq!(step(&m.take_commands()).step, Step::CheckHost);
    m.step_done(
        &key,
        attempt,
        StepDone::HostChecked(Ok(host(Pressure::Normal))),
        now,
    );
    assert_eq!(step(&m.take_commands()).step, Step::Create(own));
    m.step_done(
        &key,
        attempt,
        StepDone::Created(Ok(Some("NEW".into()))),
        now,
    );
    let commands = m.take_commands();
    // The clone is saved before it boots, so a crash mid-boot does not lose it.
    assert!(commands.iter().any(|c| matches!(
        c,
        Command::Save(Save::Assignment(ws, a)) if *ws == w.id && a.own.as_deref() == Some("NEW")
    )));
    let boot = step(&commands);
    assert!(matches!(&boot.step, Step::Boot(Target::IosOwn { udid: Some(u), .. }) if u == "NEW"));
    m.stage(&key, attempt, "booting the simulator".into());
    assert!(matches!(
        m.status(w.id, now + Duration::from_secs(3)),
        DeviceStatus::Opening { status, .. } if status == "booting the simulator… 3 s"
    ));
    finish_boot(&mut m, &key, attempt, "NEW", now);
    let commands = m.take_commands();
    assert_eq!(replies(&commands), [(token, Ok(Sess(info("NEW", "NEW"))))]);
    assert!(matches!(m.status(w.id, now), DeviceStatus::Open { .. }));
    assert_eq!(m.booted_count(), 1);
    let rows = m.rows();
    assert_eq!(rows[0].state, "open");
    assert_eq!(rows[0].clone_udid.as_deref(), Some("NEW"));
    // Open now: the next call gets the session at once.
    assert!(matches!(m.open(&key, now), OpenStart::Ready(Ok(_))));
}

#[test]
fn waiters_join_one_boot_and_stale_results_are_ignored() {
    let w = workspace("a");
    let mut m = pool(Limit::Auto, &[(&w, ios("B"))]);
    let now = Instant::now();
    let key = m.key_for(w.id).unwrap();
    let first = wait(m.open(&key, now));
    let probe = step(&m.take_commands());
    let second = wait(m.open(&key, now));
    assert!(m.take_commands().is_empty(), "joins the boot in progress");
    let running = Running {
        info: info("U", "Mine"),
        created: None,
    };
    m.step_done(
        &key,
        probe.attempt,
        StepDone::Probed(Ok(Some(running))),
        now,
    );
    let open = step(&m.take_commands());
    // A result of another attempt changes nothing.
    m.step_done(
        &key,
        probe.attempt + 100,
        StepDone::Opened(Err("x".into())),
        now,
    );
    assert!(m.take_commands().is_empty());
    m.step_done(
        &key,
        open.attempt,
        StepDone::Opened(Err("no frames".into())),
        now,
    );
    let failed = Err(OpenError::Failed("no frames".into()));
    assert_eq!(
        replies(&m.take_commands()),
        [(first, failed.clone()), (second, failed)]
    );
    assert!(matches!(
        m.status(w.id, now),
        DeviceStatus::Closed {
            error: Some("no frames"),
            ..
        }
    ));
}

#[test]
fn the_guard_refuses_under_memory_pressure() {
    let w = workspace("a");
    let mut m = pool(Limit::Auto, &[(&w, ios("B"))]);
    let now = Instant::now();
    let key = m.key_for(w.id).unwrap();
    let token = wait(m.open(&key, now));
    let b = step(&m.take_commands());
    m.step_done(&key, b.attempt, StepDone::Probed(Ok(None)), now);
    m.take_commands();
    m.step_done(
        &key,
        b.attempt,
        StepDone::HostChecked(Ok(host(Pressure::Warn))),
        now,
    );
    let replies = replies(&m.take_commands());
    let [(t, Err(OpenError::Refused(why)))] = replies.as_slice() else {
        panic!("{replies:?}");
    };
    assert_eq!(*t, token);
    assert!(
        why.starts_with("Not booting another iOS device: memory pressure is warn"),
        "{why}"
    );
    assert_eq!(m.booted_count(), 0);
}

#[test]
fn at_the_cap_the_idle_device_is_shut_down_first() {
    let (a, b) = (workspace("a"), workspace("b"));
    let mut m = pool(Limit::Manual(1), &[(&a, ios("X")), (&b, ios("Y"))]);
    let now = Instant::now();
    let (x, y) = (m.key_for(a.id).unwrap(), m.key_for(b.id).unwrap());
    assert_eq!((x.clone(), y.clone()), (base_key("X"), base_key("Y")));
    open_base(&mut m, &x, "X", now);
    m.take_commands();
    wait(m.open(&y, now));
    let probe = step(&m.take_commands());
    m.step_done(&y, probe.attempt, StepDone::Probed(Ok(None)), now);
    let commands = m.take_commands();
    let [Command::Shutdown(job)] = commands.as_slice() else {
        panic!("{commands:?}");
    };
    assert_eq!(job.key, x);
    assert!(matches!(&job.kind, ShutdownKind::Session(s) if s.0.udid == "X"));
    assert!(matches!(
        m.status(b.id, now),
        DeviceStatus::Opening { status, .. } if status.starts_with("shutting down an idle device")
    ));
    m.shutdown_finished(&x, Ok(()), now);
    assert_eq!(step(&m.take_commands()).step, Step::CheckHost);
    assert!(matches!(
        m.status(a.id, now),
        DeviceStatus::Closed { error: None, .. }
    ));
}

#[test]
fn a_failed_eviction_retries_after_a_pause() {
    let (a, b) = (workspace("a"), workspace("b"));
    let mut m = pool(Limit::Manual(1), &[(&a, ios("X")), (&b, ios("Y"))]);
    let now = Instant::now();
    let (x, y) = (base_key("X"), base_key("Y"));
    open_base(&mut m, &x, "X", now);
    m.take_commands();
    wait(m.open(&y, now));
    let probe = step(&m.take_commands());
    m.step_done(&y, probe.attempt, StepDone::Probed(Ok(None)), now);
    m.take_commands();
    m.shutdown_finished(&x, Err("simctl hung".into()), now);
    assert_eq!(step(&m.take_commands()).step, Step::Wait(SLOT_POLL));
    assert!(matches!(
        m.status(a.id, now),
        DeviceStatus::Closed {
            error: Some("Shutdown failed: simctl hung"),
            ..
        }
    ));
}

#[test]
fn a_busy_cap_waits_then_gives_up() {
    let (a, b) = (workspace("a"), workspace("b"));
    let mut m = pool(Limit::Manual(1), &[(&a, ios("X")), (&b, ios("Y"))]);
    let now = Instant::now();
    let (x, y) = (base_key("X"), base_key("Y"));
    open_base(&mut m, &x, "X", now);
    m.take_commands();
    assert_eq!(m.request_lease(&x, a.id, now), Ok(LeaseStart::Granted));
    let token = wait(m.open(&y, now));
    let probe = step(&m.take_commands());
    m.step_done(&y, probe.attempt, StepDone::Probed(Ok(None)), now);
    assert_eq!(step(&m.take_commands()).step, Step::Wait(SLOT_POLL));
    assert!(m.is_waiting(b.id));
    let later = now + Duration::from_secs(30);
    m.step_done(&y, probe.attempt, StepDone::Waited, later);
    assert_eq!(step(&m.take_commands()).step, Step::Wait(SLOT_POLL));
    let too_late = now + LEASE_TIMEOUT + Duration::from_secs(1);
    m.step_done(&y, probe.attempt, StepDone::Waited, too_late);
    assert_eq!(
        replies(&m.take_commands()),
        [(
            token,
            Err(OpenError::Failed(
                "all 1 device slots stayed busy for 10 minutes".into()
            ))
        )]
    );
    assert!(!m.is_waiting(b.id));
}

#[test]
fn an_emulator_in_the_way_is_kept_for_the_panel() {
    let w = workspace("a");
    let mut m = pool(
        Limit::Auto,
        &[(&w, Assignment::new(Platform::Android, "Pixel"))],
    );
    let now = Instant::now();
    let key = m.key_for(w.id).unwrap();
    wait(m.open(&key, now));
    let b = step(&m.take_commands());
    m.step_done(&key, b.attempt, StepDone::Probed(Ok(None)), now);
    m.take_commands();
    m.step_done(
        &key,
        b.attempt,
        StepDone::HostChecked(Err("unreadable".into())),
        now,
    );
    let boot = step(&m.take_commands());
    assert!(
        matches!(&boot.step, Step::Boot(Target::Android { avd, shared: false, .. }) if avd == "Pixel")
    );
    let conflict = Conflict {
        avd: "Pixel".into(),
        serial: "emulator-5554".into(),
    };
    let failed = BootFailed {
        message: conflict.to_string(),
        conflict: Some(conflict.clone()),
    };
    m.step_done(&key, b.attempt, StepDone::Booted(Err(failed)), now);
    m.take_commands();
    assert_eq!(m.conflict(w.id), Some(conflict));
    assert!(matches!(
        m.status(w.id, now),
        DeviceStatus::Closed {
            conflict: Some("emulator-5554"),
            ..
        }
    ));
    // A failed start keeps its entry through the launch scan.
    m.running_scanned(&RunningSet::default());
    assert!(m.conflict(w.id).is_some());
}

#[test]
fn a_call_moved_before_it_opened_asks_again() {
    let ws = workspaces(&["c1", "c2", "b2", "b4"]);
    let mut left = ios("B");
    left.own = Some("LEFT".into());
    let mut m = pool(
        Limit::Manual(2),
        &[
            (&ws[0], ios("C")),
            (&ws[1], ios("C")),
            (&ws[2], ios("B")),
            (&ws[3], left),
        ],
    );
    let t = Instant::now();
    for w in &ws[..2] {
        let start = m.request_device(w.id, t).unwrap();
        open_running(&mut m, &start.key, &w.slug, t);
    }
    let (k2, k4) = (
        PoolKey::Own(ws[2].id, Platform::Ios),
        PoolKey::Own(ws[3].id, Platform::Ios),
    );
    // Two calls of B2 hold its device; the first one's boot waits for a slot.
    assert_eq!(m.request_device(ws[2].id, t).unwrap().key, k2);
    let first = wait(m.open_held(&k2, ws[2].id, t));
    let probe = step(&m.take_commands());
    m.step_done(&k2, probe.attempt, StepDone::Probed(Ok(None)), t);
    assert_eq!(step(&m.take_commands()).step, Step::Wait(SLOT_POLL));
    let second = m.request_device(ws[2].id, t).unwrap();
    assert_eq!((&second.key, second.lease), (&k2, LeaseStart::Granted));
    assert!(m.holds(ws[2].id, &k2));
    // A device of class B runs: the boot stops and B2 no longer holds its own device.
    m.set_focused(Some(ws[3].id));
    let mut running = RunningSet::default();
    running.add(
        Platform::Ios,
        vec![RunningInstance {
            id: "LEFT".into(),
            base: None,
        }],
    );
    m.running_scanned(&running);
    m.step_done(&k2, probe.attempt, StepDone::Waited, t + SLOT_POLL);
    let commands = m.take_commands();
    assert!(matches!(
        replies(&commands).as_slice(),
        [(token, Err(OpenError::Moved { .. }))] if *token == first
    ));
    assert!(!m.holds(ws[2].id, &k2));
    // The second call, granted before the move, opens only now: it is told to ask again
    // instead of booting a device it no longer holds.
    assert!(matches!(
        m.open_held(&k2, ws[2].id, t + SLOT_POLL),
        OpenStart::Ready(Err(OpenError::Moved { .. }))
    ));
    let commands = m.take_commands();
    assert!(steps(&commands).is_empty(), "{commands:?}");
    let again = m.request_device(ws[2].id, t + SLOT_POLL).unwrap();
    assert_eq!((&again.key, again.shared), (&k4, true));
}
