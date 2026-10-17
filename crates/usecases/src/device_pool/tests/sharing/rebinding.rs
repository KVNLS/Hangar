//! A workspace leaving the device it uses, or moved to another: device_release, a new
//! assignment, single-device mode switched, a refused or rerouted boot.

use super::*;

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
    let (k2, k4) = (PoolKey::Own(ws[2].id), PoolKey::Own(ws[3].id));
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

fn android_base(id: &str) -> PoolKey {
    PoolKey::Base(DeviceKey {
        platform: Platform::Android,
        id: id.into(),
    })
}

#[test]
fn a_new_assignment_leaves_the_device_it_shared() {
    let (mut m, [a, b, c], x, t) = shared_base();
    assert_eq!(m.request_device(a.id, t).unwrap().key, x);
    let for_b = queued(m.request_device(b.id, t).map(|s| s.lease));
    let for_c = queued(m.request_device(c.id, t).map(|s| s.lease));
    m.take_commands();
    m.assign(b.id, Platform::Android, "P".into(), t);
    let commands = m.take_commands();
    assert_eq!(failed(&commands), [for_b]);
    assert!(grants(&commands).is_empty(), "a keeps the device");
    let p = android_base("P");
    assert_eq!(m.key_for(b.id), Some(p.clone()));
    let start = m.request_device(b.id, t).unwrap();
    assert_eq!((&start.key, start.lease), (&p, LeaseStart::Granted));
    assert_bound_to_held(&m);
    // The holder moves during a call: the call finishes on X, then X passes on.
    m.assign(a.id, Platform::Android, "Q".into(), t);
    assert!(grants(&m.take_commands()).is_empty(), "a call is in flight");
    let q = android_base("Q");
    assert_eq!(m.key_for(a.id), Some(q.clone()));
    assert_eq!(m.request_device(a.id, t).unwrap().key, q);
    assert_bound_to_held(&m);
    m.release_lease(&x, a.id, t + secs(1));
    assert_eq!(grants(&m.take_commands()), [(for_c, c.id, true)]);
    // A stale release of the old call changes nothing on either device.
    m.release_lease(&x, a.id, t + secs(2));
    assert!(m.take_commands().is_empty());
    assert_eq!(m.held_by_other(a.id), None);
    assert_eq!(
        m.sharing(c.id, t).and_then(|s| s.holder).map(|h| h.id),
        Some(c.id)
    );
    assert_bound_to_held(&m);
}

#[test]
fn switching_to_one_device_at_a_time_rebinds_the_workspaces() {
    let ws = workspaces(&["w1", "w2", "w3"]);
    let assigned: Vec<(&WorkspaceRef, Assignment)> = ws.iter().map(|w| (w, ios("B"))).collect();
    let mut m = pool(Limit::Manual(2), &assigned);
    let t = Instant::now();
    let k1 = PoolKey::Own(ws[0].id);
    m.request_device(ws[0].id, t).unwrap();
    m.request_device(ws[1].id, t).unwrap();
    let for_w3 = queued(m.request_device(ws[2].id, t).map(|s| s.lease));
    assert_eq!(m.key_for(ws[2].id), Some(k1.clone()));
    m.take_commands();
    m.set_limit(Limit::Manual(1), t + secs(1));
    let commands = m.take_commands();
    assert_eq!(failed(&commands), [for_w3]);
    assert!(grants(&commands).is_empty());
    let b = base_key("B");
    for w in &ws {
        assert_eq!(m.key_for(w.id), Some(b.clone()), "{}", w.name);
    }
    assert_eq!(
        m.request_device(ws[2].id, t + secs(1)).unwrap().lease,
        LeaseStart::Granted
    );
    assert_bound_to_held(&m);
    // W1's call still in flight on its own device finishes there.
    m.release_lease(&k1, ws[0].id, t + secs(2));
    assert!(grants(&m.take_commands()).is_empty());
    assert_eq!(m.key_for(ws[0].id), Some(b));
}

/// Every holder whose reservation is not ending is bound to the device it holds, and every
/// waiter to the device it waits for.
fn assert_bound_to_held(m: &Machine) {
    for (key, d) in &m.devices {
        if let Some(h) = d.leases.holding().filter(|h| !h.ending) {
            assert_eq!(m.bindings.get(&h.who), Some(key), "holder of {key:?}");
        }
        for ws in d.leases.waiters() {
            assert_eq!(m.bindings.get(ws), Some(key), "waiter of {key:?}");
        }
    }
}

#[test]
fn a_refused_boot_moves_its_waiters_before_the_lease_is_dropped() {
    let ws = workspaces(&["w1", "w2", "w3"]);
    let assigned: Vec<(&WorkspaceRef, Assignment)> = ws.iter().map(|w| (w, ios("B"))).collect();
    let mut m = pool(Limit::Auto, &assigned);
    let t = Instant::now();
    let (k1, k2) = (PoolKey::Own(ws[0].id), PoolKey::Own(ws[1].id));
    m.request_device(ws[0].id, t).unwrap();
    open_running(&mut m, &k1, "D1", t);
    assert_eq!(m.request_device(ws[1].id, t).unwrap().key, k2);
    let for_w3 = queued(m.request_lease(&k2, ws[2].id, t));
    wait(m.open(&k2, t));
    let probe = step(&m.take_commands());
    m.step_done(&k2, probe.attempt, StepDone::Probed(Ok(None)), t);
    m.take_commands();
    m.step_done(
        &k2,
        probe.attempt,
        StepDone::HostChecked(Ok(host(Pressure::Warn))),
        t,
    );
    m.take_commands();
    let moved = m.boot_refused(ws[1].id, &k2, t).unwrap().unwrap();
    assert_eq!(moved.key, k1);
    let commands = m.take_commands();
    assert!(grants(&commands).is_empty(), "{commands:?}");
    assert_eq!(
        notices(&commands),
        [
            "W3 waited for a device that could not boot: it shares D1 with W1 and takes turns.",
            "W2 could not boot its own device: it shares D1 with W1, W3 and takes turns.",
        ]
    );
    assert_eq!(m.key_for(ws[2].id), Some(k1.clone()));
    assert_bound_to_held(&m);
    m.release_lease(&k1, ws[0].id, t);
    m.release_reservation(ws[0].id, t);
    assert_eq!(grants(&m.take_commands()), [(for_w3, ws[2].id, true)]);
    assert_bound_to_held(&m);
}
