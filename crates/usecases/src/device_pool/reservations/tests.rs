//! Reservations: the holder keeps the device between its calls.

use crate::device_pool::fixtures::*;
use crate::device_pool::*;

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
    let queued_at = t + MAX_HOLD + secs(10);
    let for_b = queued(m.request_lease(&x, b.id, queued_at));
    let commands = tick(&mut m, queued_at + MAX_HOLD);
    assert!(matches!(
        commands.as_slice(),
        [Command::WarnHolder { holder, key, ends_in }]
            if *holder == a.id && *key == x && *ends_in == REVOKE_GRACE
    ));
    assert!(
        tick(&mut m, queued_at + MAX_HOLD + secs(10)).is_empty(),
        "warned once"
    );
    // The grace is over, but a call is in flight: it is not cut.
    let after_grace = queued_at + MAX_HOLD + REVOKE_GRACE;
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
fn the_hold_limit_counts_from_the_first_waiter() {
    let (mut m, [a, b, _], x, t) = shared_base();
    m.request_lease(&x, a.id, t).unwrap();
    // Alone on the device for ten minutes, then someone queues.
    let queued_at = t + secs(600);
    assert!(tick(&mut m, queued_at).is_empty());
    queued(m.request_lease(&x, b.id, queued_at));
    let sharing = m.sharing(b.id, queued_at).unwrap();
    assert_eq!(sharing.reservation_left, Some(MAX_HOLD + REVOKE_GRACE));
    assert_eq!(sharing.estimated_wait, Some(MAX_HOLD + REVOKE_GRACE));
    let commands = tick(&mut m, queued_at + secs(10));
    assert!(commands.is_empty(), "not warned at once: {commands:?}");
    assert!(tick(&mut m, queued_at + MAX_HOLD - secs(10)).is_empty());
    assert!(matches!(
        tick(&mut m, queued_at + MAX_HOLD).as_slice(),
        [Command::WarnHolder { holder, .. }] if *holder == a.id
    ));
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

#[test]
fn releasing_while_queued_leaves_the_line() {
    let (mut m, [a, b, c], x, t) = shared_base();
    m.request_lease(&x, a.id, t).unwrap();
    let reserved = queued(m.reserve(b.id, t).map(|s| s.lease));
    // A call whose budget ran out keeps its ticket on the app side.
    let call = queued(m.request_device(b.id, t).map(|s| s.lease));
    let for_c = queued(m.request_lease(&x, c.id, t + secs(1)));
    m.take_commands();
    assert!(
        !m.release_reservation(b.id, t + secs(2)),
        "b held nothing anyone waited for"
    );
    let commands = m.take_commands();
    assert_eq!(failed(&commands), [reserved, call]);
    assert!(commands.iter().all(|c| match c {
        Command::FailLease { message, .. } => message.contains("released"),
        _ => true,
    }));
    assert!(!m.is_waiting(b.id));
    assert!(!m.bindings.contains_key(&b.id));
    assert_eq!(m.sharing(c.id, t).and_then(|s| s.position), Some(0));
    m.release_lease(&x, a.id, t + secs(3));
    assert!(m.release_reservation(a.id, t + secs(3)));
    assert_eq!(grants(&m.take_commands()), [(for_c, c.id, true)]);
}
