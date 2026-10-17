//! The parked device: shut down once nothing uses it, kept for switching back.

use super::*;

fn shut_down(commands: &[Command<Sess>], key: &PoolKey) -> bool {
    commands
        .iter()
        .any(|c| matches!(c, Command::Shutdown(job) if job.key == *key))
}

#[test]
fn a_parked_device_records_its_simulator_in_the_parked_assignment() {
    let a = workspace("a");
    let mut m = pool(Limit::Auto, &[(&a, ios("B"))]);
    let t = Instant::now();
    let key = own(&a, Platform::Ios);
    let token = wait(m.open(&key, t));
    let probe = step(&m.take_commands());
    m.step_done(&key, probe.attempt, StepDone::Probed(Ok(None)), t);
    m.take_commands();
    m.step_done(
        &key,
        probe.attempt,
        StepDone::HostChecked(Ok(host(Pressure::Normal))),
        t,
    );
    assert!(matches!(step(&m.take_commands()).step, Step::Create(_)));
    // The panel's open holds no lease: the switch goes through while the simulator is made.
    let (shutdown, _) = done(m.switch_platform(a.id, android("P"), SwitchBy::Agent, t));
    assert!(shutdown.is_none(), "still starting");
    m.take_commands();
    m.step_done(
        &key,
        probe.attempt,
        StepDone::Created(Ok(Some("NEW".into()))),
        t,
    );
    // Recorded, then left unbooted: nothing uses it.
    let commands = m.take_commands();
    assert_eq!(saves(&commands), [&Save::Parked(a.id, ios_own("B", "NEW"))]);
    assert!(steps(&commands).is_empty(), "{commands:?}");
    assert_eq!(
        replies(&commands),
        [(token, Err(OpenError::Failed(TO_ANDROID.into())))]
    );
    assert_eq!(m.parked(a.id), Some(&ios_own("B", "NEW")));
    assert_eq!(m.assignment(a.id), Some(&android("P")));
    let row = m.rows().into_iter().find(|r| r.key == key).unwrap();
    assert_eq!(row.state, "off");
}

#[test]
fn a_parked_device_waiting_for_a_slot_stops_rather_than_evict() {
    let ws = workspaces(&["a", "b", "c"]);
    let [a, b, c] = [&ws[0], &ws[1], &ws[2]];
    let mut m = pool(
        Limit::Manual(2),
        &[(a, ios("B")), (b, ios("X")), (c, ios("Y"))],
    );
    let t = Instant::now();
    let kb = own(b, Platform::Ios);
    for (w, udid) in [(b, "DB"), (c, "DC")] {
        let key = m.request_device(w.id, t).unwrap().key;
        open_running(&mut m, &key, udid, t);
    }
    // The panel opens A's device: every slot is in use, it waits.
    let ka = own(a, Platform::Ios);
    let token = wait(m.open(&ka, t));
    let probe = step(&m.take_commands());
    m.step_done(&ka, probe.attempt, StepDone::Probed(Ok(None)), t);
    assert_eq!(step(&m.take_commands()).step, Step::Wait(SLOT_POLL));
    assert!(m.is_waiting(a.id));
    let (shutdown, _) = done(m.switch_platform(a.id, android("P"), SwitchBy::Agent, t));
    assert!(shutdown.is_none());
    let commands = m.take_commands();
    assert_eq!(
        replies(&commands),
        [(token, Err(OpenError::Failed(TO_ANDROID.into())))]
    );
    assert!(!commands.iter().any(|c| matches!(c, Command::Shutdown(_))));
    // B's device goes idle: nothing is evicted for the parked one.
    m.release_lease(&kb, b.id, t + secs(1));
    m.release_reservation(b.id, t + secs(1));
    m.take_commands();
    m.step_done(&ka, probe.attempt, StepDone::Waited, t + SLOT_POLL);
    assert!(m.take_commands().is_empty());
    let row = m.rows().into_iter().find(|r| r.key == ka).unwrap();
    assert_eq!(row.state, "off");
}

#[test]
fn a_parked_device_shuts_down_once_its_sharer_is_done() {
    // W's reservation expires.
    let (mut m, [a, w, ..], ka, t) = handed_over();
    m.release_lease(&ka, w.id, t + secs(1));
    assert!(!shut_down(&m.take_commands(), &ka), "W keeps it reserved");
    let commands = tick(&mut m, t + secs(1) + RESERVATION_IDLE);
    assert!(shut_down(&commands, &ka), "{commands:?}");
    m.shutdown_finished(&ka, Ok(()), t + secs(2) + RESERVATION_IDLE);
    assert_eq!(
        m.parked(a.id),
        Some(&ios_own("B", "CLONE")),
        "kept for switching back"
    );
    assert!(m.rows().iter().any(|r| r.key == ka && r.state == "off"));
    // W releases it during its call: down once the call ends.
    let (mut m, [_, w, ..], ka, t) = handed_over();
    m.release_reservation(w.id, t + secs(1));
    assert!(!shut_down(&m.take_commands(), &ka), "a call is in flight");
    m.release_lease(&ka, w.id, t + secs(2));
    assert!(shut_down(&m.take_commands(), &ka));
}

#[test]
fn a_parked_device_that_finishes_booting_is_shut_down() {
    let a = workspace("a");
    let mut m = pool(Limit::Auto, &[(&a, android("P"))]);
    let t = Instant::now();
    let key = own(&a, Platform::Android);
    let token = wait(m.open(&key, t));
    let probe = step(&m.take_commands());
    m.step_done(&key, probe.attempt, StepDone::Probed(Ok(None)), t);
    m.take_commands();
    m.step_done(
        &key,
        probe.attempt,
        StepDone::HostChecked(Ok(host(Pressure::Normal))),
        t,
    );
    assert!(matches!(step(&m.take_commands()).step, Step::Boot(_)));
    let (shutdown, _) = done(m.switch_platform(a.id, ios("B"), SwitchBy::Agent, t));
    assert!(shutdown.is_none(), "booting");
    m.take_commands();
    let running = Running {
        info: info("emulator-5554", "Pixel"),
        created: None,
    };
    m.step_done(&key, probe.attempt, StepDone::Booted(Ok(running)), t);
    let open = step(&m.take_commands());
    m.step_done(
        &key,
        open.attempt,
        StepDone::Opened(Ok((Sess(info("emulator-5554", "Pixel")), None))),
        t,
    );
    let commands = m.take_commands();
    assert!(matches!(
        replies(&commands).as_slice(),
        [(t, Ok(_))] if *t == token
    ));
    assert!(shut_down(&commands, &key), "{commands:?}");
    let mut parked = android("P");
    parked.own = Some("P".into());
    parked.serial = Some("emulator-5554".into());
    assert_eq!(saves(&commands), [&Save::Parked(a.id, parked)]);
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
