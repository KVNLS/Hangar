//! Binding: at the cap, workspaces share a running device of their class.

use hangar_core::device::{RunningInstance, RunningSet};

use crate::device_pool::fixtures::*;
use crate::device_pool::*;

fn notices(commands: &[Command<Sess>]) -> Vec<&str> {
    commands
        .iter()
        .filter_map(|c| match c {
            Command::Notice(_, n) => Some(n.as_str()),
            _ => None,
        })
        .collect()
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
    let (k1, k2) = (
        PoolKey::Own(ws[0].id, Platform::Ios),
        PoolKey::Own(ws[1].id, Platform::Ios),
    );
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
    let own = PoolKey::Own(ws[2].id, Platform::Ios);
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
    let k1 = PoolKey::Own(ws[0].id, Platform::Ios);
    m.request_device(ws[0].id, t).unwrap();
    open_running(&mut m, &k1, "D1", t);
    let own = PoolKey::Own(ws[1].id, Platform::Ios);
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
    let key = PoolKey::Own(lone.id, Platform::Ios);
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
    let (k2, k4) = (
        PoolKey::Own(ws[2].id, Platform::Ios),
        PoolKey::Own(ws[4].id, Platform::Ios),
    );
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
fn a_refused_boot_moves_its_waiters_before_the_lease_is_dropped() {
    let ws = workspaces(&["w1", "w2", "w3"]);
    let assigned: Vec<(&WorkspaceRef, Assignment)> = ws.iter().map(|w| (w, ios("B"))).collect();
    let mut m = pool(Limit::Auto, &assigned);
    let t = Instant::now();
    let (k1, k2) = (
        PoolKey::Own(ws[0].id, Platform::Ios),
        PoolKey::Own(ws[1].id, Platform::Ios),
    );
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
