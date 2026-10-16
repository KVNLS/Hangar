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
    assert_eq!(m.reserve(a.id, t), Ok(LeaseStart::Granted));
    let for_b = queued(m.reserve(b.id, t));
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
