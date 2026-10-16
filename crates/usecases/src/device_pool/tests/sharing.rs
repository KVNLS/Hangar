//! Reservations and device sharing between workspaces.

use super::*;

const fn secs(n: u64) -> Duration {
    Duration::from_secs(n)
}

/// The lease grants among `commands`: token, holder, whether it carries a hold.
fn grants(commands: &[Command<Sess>]) -> Vec<(Token, WorkspaceId, bool)> {
    commands
        .iter()
        .filter_map(|c| match c {
            Command::GrantLease {
                token,
                holder,
                hold,
                ..
            } => Some((*token, *holder, *hold)),
            _ => None,
        })
        .collect()
}

fn failed(commands: &[Command<Sess>]) -> Vec<Token> {
    commands
        .iter()
        .filter_map(|c| match c {
            Command::FailLease { token, .. } => Some(*token),
            _ => None,
        })
        .collect()
}

fn queued(start: Result<LeaseStart, String>) -> Token {
    match start {
        Ok(LeaseStart::Queued(token)) => token,
        other => panic!("not queued: {other:?}"),
    }
}

fn tick(m: &mut Machine, now: Instant) -> Vec<Command<Sess>> {
    m.monitor(now, Ok(Pressure::Normal), None);
    m.take_commands()
}

/// Three workspaces on the base device X (single-device mode), X open.
fn shared_base() -> (Machine, [WorkspaceRef; 3], PoolKey, Instant) {
    let ws = [workspace("a"), workspace("b"), workspace("c")];
    let assigned: Vec<(&WorkspaceRef, Assignment)> = ws.iter().map(|w| (w, ios("X"))).collect();
    let mut m = pool(Limit::Manual(1), &assigned);
    let now = Instant::now();
    let x = base_key("X");
    open_base(&mut m, &x, "X", now);
    m.take_commands();
    (m, ws, x, now)
}

#[test]
fn an_idle_reservation_hands_over_after_it_expires() {
    let (mut m, [a, b, _], x, t) = shared_base();
    assert_eq!(m.request_lease(&x, a.id, t), Ok(LeaseStart::Granted));
    let for_b = queued(m.request_lease(&x, b.id, t));
    // The call ended, but `a` keeps the device between its calls.
    m.release_lease(&x, a.id, t + secs(5));
    assert!(grants(&m.take_commands()).is_empty());
    assert_eq!(m.held_by_other(b.id).map(|(ws, _)| ws), Some(a.id));
    assert!(grants(&tick(&mut m, t + secs(5) + RESERVATION_IDLE - secs(1))).is_empty());
    assert_eq!(
        grants(&tick(&mut m, t + secs(5) + RESERVATION_IDLE)),
        [(for_b, b.id, true)]
    );
}

#[test]
fn no_reservation_expires_while_a_call_is_in_flight() {
    let (mut m, [a, ..], x, t) = shared_base();
    m.request_lease(&x, a.id, t).unwrap();
    let commands = tick(&mut m, t + secs(3600));
    assert!(commands.is_empty(), "{commands:?}");
    assert!(m.rows()[0].busy);
    m.release_lease(&x, a.id, t + secs(3600));
    tick(&mut m, t + secs(3600) + RESERVATION_IDLE);
    assert!(!m.rows()[0].busy, "expired with nobody waiting: idle again");
}

#[test]
fn activity_extends_the_reservation() {
    let (mut m, [a, b, _], x, t) = shared_base();
    m.request_lease(&x, a.id, t).unwrap();
    let for_b = queued(m.request_lease(&x, b.id, t));
    m.release_lease(&x, a.id, t);
    assert!(m.touch(a.id, t + secs(60)));
    assert!(grants(&tick(&mut m, t + RESERVATION_IDLE)).is_empty());
    assert_eq!(
        grants(&tick(&mut m, t + secs(60) + RESERVATION_IDLE)),
        [(for_b, b.id, true)]
    );
}

#[test]
fn the_holder_calls_again_within_its_reservation() {
    let (mut m, [a, b, _], x, t) = shared_base();
    assert_eq!(m.request_lease(&x, a.id, t), Ok(LeaseStart::Granted));
    assert_eq!(m.request_lease(&x, a.id, t), Ok(LeaseStart::Granted));
    let for_b = queued(m.request_lease(&x, b.id, t));
    m.release_lease(&x, a.id, t);
    m.release_lease(&x, a.id, t + secs(1));
    // Between its calls `a` keeps the device although `b` waits.
    assert_eq!(
        m.request_lease(&x, a.id, t + secs(2)),
        Ok(LeaseStart::Granted)
    );
    m.release_lease(&x, a.id, t + secs(3));
    assert!(grants(&m.take_commands()).is_empty());
    assert!(m.release_reservation(a.id, t + secs(4)), "b was waiting");
    assert_eq!(grants(&m.take_commands()), [(for_b, b.id, true)]);
}

#[test]
fn the_hold_limit_warns_once_and_revokes_between_calls() {
    let (mut m, [a, b, _], x, t) = shared_base();
    m.request_lease(&x, a.id, t).unwrap();
    // Nobody waits: no limit.
    assert!(tick(&mut m, t + MAX_HOLD + secs(10)).is_empty());
    let for_b = queued(m.request_lease(&x, b.id, t + MAX_HOLD + secs(10)));
    let commands = tick(&mut m, t + MAX_HOLD + secs(20));
    assert!(matches!(
        commands.as_slice(),
        [Command::WarnHolder { holder, key, ends_in }]
            if *holder == a.id && *key == x && *ends_in == REVOKE_GRACE
    ));
    assert!(
        tick(&mut m, t + MAX_HOLD + secs(30)).is_empty(),
        "warned once"
    );
    // The grace is over, but a call is in flight: it is not cut.
    let after_grace = t + MAX_HOLD + secs(20) + REVOKE_GRACE;
    assert!(tick(&mut m, after_grace).is_empty());
    m.release_lease(&x, a.id, after_grace + secs(5));
    let commands = m.take_commands();
    assert!(matches!(
        commands.first(),
        Some(Command::Revoked { holder, key }) if *holder == a.id && *key == x
    ));
    assert_eq!(grants(&commands), [(for_b, b.id, true)]);
}

#[test]
fn a_polled_ticket_survives_the_lease_timeout() {
    let (mut m, [a, b, c], x, t) = shared_base();
    m.request_lease(&x, a.id, t).unwrap();
    let for_b = queued(m.request_lease(&x, b.id, t));
    let for_c = queued(m.request_lease(&x, c.id, t));
    assert!(m.touch(b.id, t + secs(9 * 60)));
    assert_eq!(failed(&tick(&mut m, t + LEASE_TIMEOUT)), [for_c]);
    assert!(m.is_waiting(b.id));
    assert!(failed(&tick(&mut m, t + secs(9 * 60) + LEASE_TIMEOUT - secs(1))).is_empty());
    assert_eq!(
        failed(&tick(&mut m, t + secs(9 * 60) + LEASE_TIMEOUT)),
        [for_b]
    );
    assert!(!m.is_waiting(b.id));
}

#[test]
fn a_reservation_without_a_call_is_queued_and_released() {
    let (mut m, [a, b, c], x, t) = shared_base();
    assert_eq!(m.reserve(a.id, t).map(|s| s.lease), Ok(LeaseStart::Granted));
    let for_b = queued(m.reserve(b.id, t).map(|s| s.lease));
    let for_c = queued(m.request_lease(&x, c.id, t));
    assert!(m.release_reservation(a.id, t));
    // A reservation carries no hold: `b` keeps it idle until it expires.
    assert_eq!(grants(&m.take_commands()), [(for_b, b.id, false)]);
    assert_eq!(
        grants(&tick(&mut m, t + RESERVATION_IDLE)),
        [(for_c, c.id, true)]
    );
    assert!(
        !m.release_reservation(c.id, t + RESERVATION_IDLE),
        "a call is in flight"
    );
}

#[test]
fn taking_over_revokes_the_holder() {
    let (mut m, [a, b, c], x, t) = shared_base();
    m.request_lease(&x, a.id, t).unwrap();
    let for_b = queued(m.request_lease(&x, b.id, t));
    // `a` is between calls: `c` gets the device now, ahead of `b`.
    m.release_lease(&x, a.id, t);
    m.take_commands();
    assert_eq!(m.take_over(&x, c.id, t), Ok(LeaseStart::Granted));
    let commands = m.take_commands();
    assert!(matches!(
        commands.as_slice(),
        [Command::Revoked { holder, .. }] if *holder == a.id
    ));
    assert_eq!(m.held_by_other(a.id).map(|(ws, _)| ws), Some(c.id));
    // `c` is taken over in turn during a call of `c`: the call ends first.
    m.request_lease(&x, c.id, t).unwrap();
    let for_a = queued(m.take_over(&x, a.id, t + secs(1)));
    assert!(matches!(
        m.take_commands().as_slice(),
        [Command::Revoked { holder, .. }] if *holder == c.id
    ));
    let again = queued(m.request_lease(&x, c.id, t + secs(2)));
    m.release_lease(&x, c.id, t + secs(3));
    assert_eq!(grants(&m.take_commands()), [(for_a, a.id, false)]);
    assert!(m.release_reservation(a.id, t + secs(4)));
    assert_eq!(grants(&m.take_commands()), [(for_b, b.id, true)]);
    m.release_lease(&x, b.id, t + secs(5));
    assert!(m.release_reservation(b.id, t + secs(5)));
    assert_eq!(grants(&m.take_commands()), [(again, c.id, true)]);
}

#[test]
fn a_reserved_device_is_not_shut_down_to_make_room() {
    let (a, b) = (workspace("a"), workspace("b"));
    let mut m = pool(Limit::Manual(1), &[(&a, ios("X")), (&b, ios("Y"))]);
    let t = Instant::now();
    let (x, y) = (base_key("X"), base_key("Y"));
    open_base(&mut m, &x, "X", t);
    m.take_commands();
    m.request_lease(&x, a.id, t).unwrap();
    m.release_lease(&x, a.id, t);
    m.take_commands();
    wait(m.open(&y, t));
    let probe = step(&m.take_commands());
    m.step_done(&y, probe.attempt, StepDone::Probed(Ok(None)), t);
    assert_eq!(step(&m.take_commands()).step, Step::Wait(SLOT_POLL));
    tick(&mut m, t + RESERVATION_IDLE);
    m.step_done(&y, probe.attempt, StepDone::Waited, t + RESERVATION_IDLE);
    let commands = m.take_commands();
    assert!(
        matches!(commands.as_slice(), [Command::Shutdown(job)] if job.key == x),
        "{commands:?}"
    );
}

fn notices(commands: &[Command<Sess>]) -> Vec<&str> {
    commands
        .iter()
        .filter_map(|c| match c {
            Command::Notice(_, n) => Some(n.as_str()),
            _ => None,
        })
        .collect()
}

/// Opens `key`, found already running as `udid`.
fn open_running(m: &mut Machine, key: &PoolKey, udid: &str, now: Instant) {
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

fn workspaces(names: &[&str]) -> Vec<WorkspaceRef> {
    names.iter().map(|n| workspace(n)).collect()
}

#[test]
fn at_the_cap_workspaces_share_the_devices_of_their_class() {
    let ws = workspaces(&["w1", "w2", "w3", "w4", "w5"]);
    let assigned: Vec<(&WorkspaceRef, Assignment)> = ws.iter().map(|w| (w, ios("B"))).collect();
    let mut m = pool(Limit::Manual(2), &assigned);
    let t = Instant::now();
    let starts: Vec<DeviceStart> = ws
        .iter()
        .map(|w| m.request_device(w.id, t).unwrap())
        .collect();
    let (k1, k2) = (PoolKey::Own(ws[0].id), PoolKey::Own(ws[1].id));
    assert_eq!(
        starts[0],
        DeviceStart {
            key: k1.clone(),
            lease: LeaseStart::Granted,
            shared: false,
            stale_app: None
        }
    );
    assert_eq!((&starts[1].key, starts[1].shared), (&k2, false));
    // The other three wait for the two devices about to boot, the shortest queue first.
    let keys: Vec<&PoolKey> = starts[2..].iter().map(|s| &s.key).collect();
    assert_eq!(keys, [&k1, &k2, &k1]);
    assert!(
        starts[2..]
            .iter()
            .all(|s| s.shared && matches!(s.lease, LeaseStart::Queued(_)))
    );
    let commands = m.take_commands();
    assert_eq!(
        notices(&commands),
        [
            "All 2 device slots are in use: W3 shares Hangar · app/w1 with W1 and takes turns.",
            "All 2 device slots are in use: W4 shares Hangar · app/w2 with W2 and takes turns.",
            "All 2 device slots are in use: W5 shares Hangar · app/w1 with W1, W3 and takes \
             turns.",
        ]
    );
    open_running(&mut m, &k1, "D1", t);
    open_running(&mut m, &k2, "D2", t);
    assert_eq!(m.booted_count(), 2);
    assert_eq!(m.key_for(ws[4].id), Some(k1.clone()));
    assert!(matches!(m.status(ws[4].id, t), DeviceStatus::Open { .. }));
    let sharing = m.sharing(ws[4].id, t).unwrap();
    let member = |w: &WorkspaceRef| Member {
        id: w.id,
        name: w.name.clone(),
    };
    assert_eq!(
        sharing,
        Sharing {
            key: k1.clone(),
            device: "D1".into(),
            device_id: Some("D1".into()),
            holder: Some(member(&ws[0])),
            queue: vec![member(&ws[2]), member(&ws[4])],
            position: Some(1),
            shared_with: vec!["W1".into(), "W3".into()],
            reservation_left: Some(MAX_HOLD + REVOKE_GRACE),
            estimated_wait: Some(MAX_HOLD + REVOKE_GRACE + MAX_HOLD),
            stale_app_from: None,
        }
    );
    let row = m.rows().into_iter().find(|r| r.key == k1).unwrap();
    assert_eq!((row.holder.as_deref(), row.queue_len), (Some("W1"), 2));
    assert_eq!(row.workspaces.len(), 3);
    assert!(
        m.take_over(&k2, ws[2].id, t).is_err(),
        "W3 uses the other device"
    );
    // W1 is done: W3 gets the device.
    m.release_lease(&k1, ws[0].id, t);
    m.release_reservation(ws[0].id, t);
    let LeaseStart::Queued(for_w3) = starts[2].lease else {
        unreachable!()
    };
    assert_eq!(grants(&m.take_commands()), [(for_w3, ws[2].id, true)]);
    assert_eq!(
        m.sharing(ws[0].id, t).map(|s| s.holder.map(|h| h.id)),
        Some(Some(ws[2].id))
    );
    // A new call of W1 now waits behind the workspaces sharing its device.
    assert!(matches!(
        m.request_device(ws[0].id, t).map(|s| s.lease),
        Ok(LeaseStart::Queued(_))
    ));
}

#[test]
fn another_class_is_never_shared() {
    let ws = workspaces(&["w1", "w2", "c1"]);
    let mut m = pool(
        Limit::Manual(2),
        &[(&ws[0], ios("B")), (&ws[1], ios("B")), (&ws[2], ios("C"))],
    );
    let t = Instant::now();
    for w in &ws[..2] {
        let start = m.request_device(w.id, t).unwrap();
        open_running(&mut m, &start.key, &w.slug, t);
    }
    let own = PoolKey::Own(ws[2].id);
    let start = m.request_device(ws[2].id, t).unwrap();
    assert_eq!((&start.key, start.shared), (&own, false));
    assert!(notices(&m.take_commands()).is_empty());
    assert_eq!(m.borrowable(Platform::Ios, "C", &own), None);
    // Its own device waits for a slot.
    wait(m.open(&own, t));
    let probe = step(&m.take_commands());
    m.step_done(&own, probe.attempt, StepDone::Probed(Ok(None)), t);
    assert_eq!(step(&m.take_commands()).step, Step::Wait(SLOT_POLL));
    assert!(m.is_waiting(ws[2].id));
}

#[test]
fn a_refused_boot_shares_a_running_device() {
    let ws = workspaces(&["w1", "w2"]);
    let mut m = pool(Limit::Auto, &[(&ws[0], ios("B")), (&ws[1], ios("B"))]);
    let t = Instant::now();
    let k1 = PoolKey::Own(ws[0].id);
    m.request_device(ws[0].id, t).unwrap();
    open_running(&mut m, &k1, "D1", t);
    let own = PoolKey::Own(ws[1].id);
    let start = m.request_device(ws[1].id, t).unwrap();
    assert_eq!((&start.key, start.lease), (&own, LeaseStart::Granted));
    wait(m.open(&own, t));
    let probe = step(&m.take_commands());
    m.step_done(&own, probe.attempt, StepDone::Probed(Ok(None)), t);
    m.take_commands();
    m.step_done(
        &own,
        probe.attempt,
        StepDone::HostChecked(Ok(host(Pressure::Warn))),
        t,
    );
    assert!(matches!(
        replies(&m.take_commands()).as_slice(),
        [(_, Err(OpenError::Refused(_)))]
    ));
    let moved = m.boot_refused(ws[1].id, &own, t).unwrap().unwrap();
    assert_eq!((&moved.key, moved.shared), (&k1, true));
    let LeaseStart::Queued(token) = moved.lease else {
        panic!("W1 holds D1: {moved:?}");
    };
    assert_eq!(
        notices(&m.take_commands()),
        ["W2 could not boot its own device: it shares D1 with W1 and takes turns."]
    );
    // The lease on the refused device is no longer W2's: dropping it changes nothing.
    m.release_lease(&own, ws[1].id, t);
    assert!(m.take_commands().is_empty());
    m.release_lease(&k1, ws[0].id, t);
    m.release_reservation(ws[0].id, t);
    assert_eq!(grants(&m.take_commands()), [(token, ws[1].id, true)]);
    // No compatible device runs: nothing to share.
    let lone = workspace("lone");
    m.track(lone.clone());
    m.resolved(lone.id, ios("Z"));
    let key = PoolKey::Own(lone.id);
    m.request_device(lone.id, t).unwrap();
    assert_eq!(m.boot_refused(lone.id, &key, t), Ok(None));
    assert_eq!(m.key_for(lone.id), Some(key));
}

#[test]
fn a_workspace_waiting_for_a_slot_moves_to_a_compatible_device() {
    let ws = workspaces(&["c1", "c2", "b2", "b3", "b4"]);
    let mut left = ios("B");
    left.own = Some("LEFT".into());
    let mut m = pool(
        Limit::Manual(2),
        &[
            (&ws[0], ios("C")),
            (&ws[1], ios("C")),
            (&ws[2], ios("B")),
            (&ws[3], ios("B")),
            (&ws[4], left),
        ],
    );
    let t = Instant::now();
    for w in &ws[..2] {
        let start = m.request_device(w.id, t).unwrap();
        open_running(&mut m, &start.key, &w.slug, t);
    }
    let (k2, k4) = (PoolKey::Own(ws[2].id), PoolKey::Own(ws[4].id));
    assert_eq!(m.request_device(ws[2].id, t).unwrap().key, k2);
    let start = m.request_device(ws[3].id, t).unwrap();
    assert_eq!((&start.key, start.shared), (&k2, true), "about to boot");
    let LeaseStart::Queued(for_b3) = start.lease else {
        panic!("{start:?}");
    };
    m.take_commands();
    let opening = wait(m.open(&k2, t));
    let probe = step(&m.take_commands());
    m.step_done(&k2, probe.attempt, StepDone::Probed(Ok(None)), t);
    assert_eq!(step(&m.take_commands()).step, Step::Wait(SLOT_POLL));
    // A device of class B turns out to be running (shown, so it stays).
    m.set_focused(Some(ws[4].id));
    let mut running = RunningSet::default();
    running.add(
        Platform::Ios,
        vec![RunningInstance {
            id: "LEFT".into(),
            base: None,
        }],
    );
    m.running_scanned(&running);
    assert!(m.take_commands().is_empty());
    m.step_done(&k2, probe.attempt, StepDone::Waited, t + SLOT_POLL);
    let commands = m.take_commands();
    assert!(
        commands.iter().any(|c| matches!(
            c,
            Command::GrantLease { token, key, .. } if *token == for_b3 && *key == k4
        )),
        "{commands:?}"
    );
    assert!(matches!(
        replies(&commands).as_slice(),
        [(token, Err(OpenError::Moved { to, .. }))] if *token == opening && *to == k4
    ));
    assert!(steps(&commands).is_empty(), "the bring-up stopped");
    assert_eq!(m.key_for(ws[3].id), Some(k4.clone()));
    assert!(!m.is_waiting(ws[2].id));
    // Asked again, B2 shares it too.
    let again = m.request_device(ws[2].id, t + SLOT_POLL).unwrap();
    assert_eq!((&again.key, again.shared), (&k4, true));
    assert!(matches!(again.lease, LeaseStart::Queued(_)));
}

#[test]
fn a_grant_says_whose_app_is_on_the_device() {
    let (mut m, [a, b, _], x, t) = shared_base();
    assert_eq!(m.request_device(a.id, t).unwrap().key, x);
    let for_b = queued(m.request_device(b.id, t).map(|s| s.lease));
    m.app_installed(&x, a.id);
    m.release_lease(&x, a.id, t);
    m.release_reservation(a.id, t);
    let commands = m.take_commands();
    assert!(matches!(
        commands.as_slice(),
        [Command::GrantLease { token, stale_app: Some(from), .. }]
            if *token == for_b && from == "A"
    ));
    assert_eq!(
        m.sharing(b.id, t).and_then(|s| s.stale_app_from),
        Some("A".into())
    );
    m.app_installed(&x, b.id);
    assert_eq!(m.sharing(b.id, t).and_then(|s| s.stale_app_from), None);
    m.release_lease(&x, b.id, t);
    m.release_reservation(b.id, t);
    let start = m.request_device(a.id, t).unwrap();
    assert_eq!(start.stale_app.as_deref(), Some("B"));
}

#[test]
fn archiving_drops_the_binding_and_fails_each_ticket_once() {
    let ws = workspaces(&["w1", "w2", "w3", "w4"]);
    let assigned: Vec<(&WorkspaceRef, Assignment)> = ws.iter().map(|w| (w, ios("B"))).collect();
    let mut m = pool(Limit::Manual(2), &assigned);
    let t = Instant::now();
    let k1 = PoolKey::Own(ws[0].id);
    m.request_device(ws[0].id, t).unwrap();
    m.request_device(ws[1].id, t).unwrap();
    let w3: Vec<Token> = (0..2)
        .map(|_| queued(m.request_device(ws[2].id, t).map(|s| s.lease)))
        .collect();
    let w4 = queued(m.request_device(ws[3].id, t).map(|s| s.lease));
    assert_eq!(m.key_for(ws[2].id), Some(k1.clone()));
    assert_eq!(
        m.key_for(ws[3].id),
        Some(PoolKey::Own(ws[1].id)),
        "shortest queue"
    );
    m.take_commands();
    assert!(m.archive(ws[2].id, t));
    assert_eq!(failed(&m.take_commands()), w3);
    assert_eq!(m.key_for(ws[2].id), Some(PoolKey::Own(ws[2].id)));
    // The owner is archived: who shares its device loses it.
    assert!(m.archive(ws[1].id, t));
    let commands = m.take_commands();
    assert_eq!(failed(&commands), [w4]);
    assert!(commands.iter().any(|c| matches!(
        c,
        Command::FailLease { message, .. } if message == "Hangar · app/w2 was shut down with its workspace"
    )));
    assert_eq!(m.key_for(ws[3].id), Some(PoolKey::Own(ws[3].id)));
    assert!(grants(&commands).is_empty());
}

#[test]
fn a_grant_returned_by_a_caller_that_gave_up_keeps_a_reservation() {
    let (mut m, [a, b, c], x, t) = shared_base();
    assert_eq!(
        m.request_device(a.id, t).unwrap().lease,
        LeaseStart::Granted
    );
    // `b`'s call stops waiting on its side: its ticket stays in line, ahead of `c`.
    let for_b = match m.request_device(b.id, t).unwrap().lease {
        LeaseStart::Queued(token) => token,
        other => panic!("not queued: {other:?}"),
    };
    let for_c = queued(m.request_lease(&x, c.id, t + secs(1)));
    m.release_lease(&x, a.id, t + secs(2));
    assert!(m.release_reservation(a.id, t + secs(3)));
    assert_eq!(grants(&m.take_commands()), [(for_b, b.id, true)]);
    // Nobody takes the grant: the lease comes back, `b` keeps the device between calls.
    m.release_lease(&x, b.id, t + secs(3));
    assert!(grants(&m.take_commands()).is_empty());
    assert_eq!(m.held_by_other(c.id).map(|(ws, _)| ws), Some(b.id));
    let next = m.request_device(b.id, t + secs(20)).unwrap();
    assert_eq!((next.key, next.lease), (x.clone(), LeaseStart::Granted));
    m.release_lease(&x, b.id, t + secs(21));
    assert_eq!(
        grants(&tick(&mut m, t + secs(21) + RESERVATION_IDLE)),
        [(for_c, c.id, true)]
    );
}
