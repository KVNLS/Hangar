//! Switching a workspace to its other platform: one platform attached at a time.

use super::*;

mod parked;

const SWITCHED: &str = "this workspace switched to Android; call again";
/// What an open of a parked device left unstarted gets.
const TO_ANDROID: &str = "this workspace switched to Android";

fn android(base: &str) -> Assignment {
    Assignment::new(Platform::Android, base)
}

fn ios_own(base: &str, own: &str) -> Assignment {
    let mut a = ios(base);
    a.own = Some(own.into());
    a
}

fn own(ws: &WorkspaceRef, platform: Platform) -> PoolKey {
    PoolKey::Own(ws.id, platform)
}

/// The jobs of a switch that went through.
fn done(
    switched: Result<Switched<Sess>, String>,
) -> (Option<ShutdownJob<Sess>>, Option<Replaced<Sess>>) {
    match switched {
        Ok(Switched::Done { shutdown, replaced }) => (shutdown, replaced),
        other => panic!("not switched: {other:?}"),
    }
}

fn saves(commands: &[Command<Sess>]) -> Vec<&Save> {
    commands
        .iter()
        .filter_map(|c| match c {
            Command::Save(save) => Some(save),
            _ => None,
        })
        .collect()
}

#[test]
fn a_switch_hands_the_device_over_and_shares_the_new_platform_at_the_cap() {
    let ws = workspaces(&["a", "w", "c"]);
    let [a, w, c] = [&ws[0], &ws[1], &ws[2]];
    let mut m = pool(
        Limit::Manual(2),
        &[(a, ios("B")), (w, ios("B")), (c, android("P"))],
    );
    let t = Instant::now();
    let (ka, kc) = (own(a, Platform::Ios), own(c, Platform::Android));
    assert_eq!(m.request_device(a.id, t).unwrap().key, ka);
    open_running(&mut m, &ka, "DA", t);
    assert_eq!(m.request_device(c.id, t).unwrap().key, kc);
    open_running(&mut m, &kc, "DC", t);
    let start = m.request_device(w.id, t).unwrap();
    assert_eq!((&start.key, start.shared), (&ka, true));
    let for_w = queued(Ok(start.lease));
    m.release_lease(&ka, a.id, t + secs(1));
    m.take_commands();
    let (shutdown, replaced) =
        done(m.switch_platform(a.id, android("P"), SwitchBy::Agent, t + secs(2)));
    assert!(shutdown.is_none(), "W uses the old device: it stays");
    assert!(replaced.is_none());
    let commands = m.take_commands();
    assert_eq!(grants(&commands), [(for_w, w.id, true)]);
    assert_eq!(
        saves(&commands),
        [
            &Save::Parked(a.id, ios("B")),
            &Save::Assignment(a.id, android("P")),
        ]
    );
    assert_eq!(m.parked(a.id), Some(&ios("B")));
    assert_eq!(m.assignment(a.id), Some(&android("P")));
    assert_eq!(m.key_for(a.id), Some(own(a, Platform::Android)));
    let row = m.rows().into_iter().find(|r| r.key == ka).unwrap();
    assert_eq!((row.state, row.holder.as_deref()), ("open", Some("W")));
    // Both slots are in use: A shares C's Android device and waits for its turn.
    let start = m.reserve(a.id, t + secs(2)).unwrap();
    assert_eq!((&start.key, start.shared), (&kc, true));
    assert!(matches!(start.lease, LeaseStart::Queued(_)));
    assert_bound_to_held(&m);
}

#[test]
fn a_switch_waits_for_the_calls_and_runs_in_flight() {
    let a = workspace("a");
    let mut m = pool(Limit::Auto, &[(&a, ios("B"))]);
    let t = Instant::now();
    let key = own(&a, Platform::Ios);
    // A run and a tool call.
    m.request_device(a.id, t).unwrap();
    m.request_device(a.id, t).unwrap();
    m.take_commands();
    let refused: Result<(), String> =
        Err("a call or build still uses Hangar · app/a; switch when it ends".into());
    assert_eq!(
        m.switch_platform(a.id, android("P"), SwitchBy::Agent, t)
            .map(|_| ()),
        refused
    );
    m.release_lease(&key, a.id, t + secs(1));
    assert_eq!(
        m.switch_platform(a.id, android("P"), SwitchBy::Agent, t + secs(1))
            .map(|_| ()),
        refused
    );
    assert!(m.take_commands().is_empty(), "nothing changed");
    assert_eq!(m.assignment(a.id), Some(&ios("B")));
    m.release_lease(&key, a.id, t + secs(2));
    done(m.switch_platform(a.id, android("P"), SwitchBy::Agent, t + secs(2)));
    assert_eq!(m.assignment(a.id), Some(&android("P")));
}

#[test]
fn switching_back_reuses_the_parked_simulator() {
    let a = workspace("a");
    let mut m = pool(Limit::Auto, &[(&a, ios_own("B", "CLONE"))]);
    let t = Instant::now();
    let key = own(&a, Platform::Ios);
    open_running(&mut m, &key, "CLONE", t);
    // Nobody else uses it: it is shut down, not deleted.
    let (shutdown, replaced) = done(m.switch_platform(a.id, android("P"), SwitchBy::Agent, t));
    assert_eq!(shutdown.map(|job| job.key), Some(key.clone()));
    assert!(replaced.is_none());
    m.shutdown_finished(&key, Ok(()), t);
    m.take_commands();
    assert_eq!(m.parked(a.id), Some(&ios_own("B", "CLONE")));
    // The same base, without its simulator: the parked one is taken back.
    let (shutdown, replaced) =
        done(m.switch_platform(a.id, ios("B"), SwitchBy::Agent, t + secs(1)));
    assert!(shutdown.is_none() && replaced.is_none());
    assert_eq!(m.assignment(a.id), Some(&ios_own("B", "CLONE")));
    assert_eq!(m.parked(a.id), Some(&android("P")));
    wait(m.open(&key, t + secs(1)));
    let probe = step(&m.take_commands());
    let clone = Target::IosOwn {
        udid: Some("CLONE".into()),
        base: "B".into(),
        name: "Hangar · app/a".into(),
    };
    assert_eq!(probe.step, Step::Probe(clone.clone()));
    m.step_done(&key, probe.attempt, StepDone::Probed(Ok(None)), t);
    m.take_commands();
    m.step_done(
        &key,
        probe.attempt,
        StepDone::HostChecked(Ok(host(Pressure::Normal))),
        t,
    );
    // The simulator exists: nothing is created or saved.
    assert_eq!(step(&m.take_commands()).step, Step::Create(clone.clone()));
    m.step_done(&key, probe.attempt, StepDone::Created(Ok(None)), t);
    let commands = m.take_commands();
    assert!(saves(&commands).is_empty(), "{commands:?}");
    assert_eq!(step(&commands).step, Step::Boot(clone));
}

#[test]
fn switching_back_to_another_base_drops_the_parked_simulator() {
    let a = workspace("a");
    let mut m = pool(Limit::Auto, &[(&a, ios_own("B", "CLONE"))]);
    let t = Instant::now();
    done(m.switch_platform(a.id, android("P"), SwitchBy::Agent, t));
    let (shutdown, replaced) = done(m.switch_platform(a.id, ios("C"), SwitchBy::Agent, t));
    assert!(shutdown.is_none(), "the Android device never ran");
    let replaced = replaced.unwrap();
    assert_eq!(replaced.clone.as_deref(), Some("CLONE"));
    assert!(replaced.shutdown.is_none());
    assert_eq!(m.assignment(a.id), Some(&ios("C")));
    assert_eq!(m.parked(a.id), Some(&android("P")));
}

#[test]
fn the_same_platform_is_no_change() {
    let a = workspace("a");
    let mut m = pool(Limit::Auto, &[(&a, ios("B"))]);
    let t = Instant::now();
    m.request_device(a.id, t).unwrap();
    m.take_commands();
    assert!(matches!(
        m.switch_platform(a.id, ios("C"), SwitchBy::Agent, t),
        Ok(Switched::Same)
    ));
    assert!(m.take_commands().is_empty());
    assert_eq!(m.assignment(a.id), Some(&ios("B")));
    assert_eq!(m.parked(a.id), None);
    assert!(
        m.holds(a.id, &own(&a, Platform::Ios)),
        "the call keeps its device"
    );
}

#[test]
fn the_waiting_calls_of_the_switching_workspace_fail() {
    let (mut m, [a, b, c], x, t) = shared_base();
    m.request_lease(&x, a.id, t).unwrap();
    let call = queued(m.request_device(b.id, t).map(|s| s.lease));
    let reserved = queued(m.reserve(b.id, t).map(|s| s.lease));
    let for_c = queued(m.request_lease(&x, c.id, t));
    m.take_commands();
    let (shutdown, replaced) =
        done(m.switch_platform(b.id, android("P"), SwitchBy::Agent, t + secs(1)));
    assert!(shutdown.is_none() && replaced.is_none());
    let commands = m.take_commands();
    assert_eq!(failed(&commands), [call, reserved]);
    assert!(commands.iter().all(|c| match c {
        Command::FailLease { message, .. } => message == SWITCHED,
        _ => true,
    }));
    assert!(grants(&commands).is_empty(), "a keeps X");
    assert!(!m.is_waiting(b.id));
    assert_eq!(m.sharing(c.id, t).and_then(|s| s.position), Some(0));
    // One device at a time: b's Android device is the base one.
    let p = PoolKey::Base(DeviceKey {
        platform: Platform::Android,
        id: "P".into(),
    });
    assert_eq!(m.key_for(b.id), Some(p));
    m.release_lease(&x, a.id, t + secs(2));
    m.release_reservation(a.id, t + secs(2));
    assert_eq!(grants(&m.take_commands()), [(for_c, c.id, true)]);
}

#[test]
fn a_parked_simulator_is_listed_and_can_be_deleted() {
    let a = workspace("a");
    let mut m = pool(Limit::Auto, &[(&a, android("P"))]);
    m.adopt(a.id, None, Some(ios_own("B", "CLONE")));
    let rows = m.rows();
    let [row] = rows.as_slice() else {
        panic!("{rows:?}");
    };
    assert_eq!(row.key, own(&a, Platform::Ios));
    assert_eq!(
        (row.state, row.clone_udid.as_deref()),
        ("not open", Some("CLONE"))
    );
    assert!(row.workspaces.is_empty(), "not attached");
    let (udid, shutdown) = m.delete_clone(&row.key).unwrap();
    assert_eq!((udid.as_str(), shutdown.is_none()), ("CLONE", true));
    assert_eq!(saves(&m.take_commands()), [&Save::Parked(a.id, ios("B"))]);
    assert_eq!(m.parked(a.id), Some(&ios("B")));
    assert!(m.rows().is_empty());
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
fn assigning_the_parked_platform_takes_the_parked_assignment_back() {
    let a = workspace("a");
    let mut m = pool(Limit::Auto, &[(&a, ios_own("B", "CLONE"))]);
    let t = Instant::now();
    let android_key = own(&a, Platform::Android);
    done(m.switch_platform(a.id, android("P"), SwitchBy::Agent, t));
    open_running(&mut m, &android_key, "emulator-5554", t);
    // Another base: the parked simulator is deleted, the Android device shut down.
    m.take_commands();
    let replaced = m
        .assign(a.id, Platform::Ios, "C".into(), t)
        .unwrap()
        .unwrap();
    assert_eq!(replaced.clone.as_deref(), Some("CLONE"));
    assert!(replaced.shutdown.is_none(), "the simulator never ran");
    let commands = m.take_commands();
    assert!(commands.iter().any(|c| matches!(
        c,
        Command::Shutdown(job) if job.key == android_key
    )));
    assert!(
        saves(&commands).contains(&&Save::Unparked(a.id)),
        "{commands:?}"
    );
    assert_eq!(m.assignment(a.id), Some(&ios("C")));
    assert_eq!(m.parked(a.id), None);
    // The same base keeps the parked simulator.
    let mut m = pool(Limit::Auto, &[(&a, ios_own("B", "CLONE"))]);
    done(m.switch_platform(a.id, android("P"), SwitchBy::Agent, t));
    let replaced = m
        .assign(a.id, Platform::Ios, "B".into(), t)
        .unwrap()
        .unwrap();
    assert_eq!(replaced.clone, None);
    assert_eq!(m.assignment(a.id), Some(&ios_own("B", "CLONE")));
    assert_eq!(m.parked(a.id), None);
}

#[test]
fn switching_back_saves_the_other_parked_assignment() {
    let a = workspace("a");
    let mut m = pool(Limit::Auto, &[(&a, ios_own("B", "CLONE"))]);
    let t = Instant::now();
    done(m.switch_platform(a.id, android("P"), SwitchBy::Agent, t));
    m.take_commands();
    done(m.switch_platform(a.id, ios("B"), SwitchBy::Agent, t));
    // The parked Android assignment replaces the saved iOS one: nothing is forgotten.
    assert_eq!(
        saves(&m.take_commands()),
        [
            &Save::Parked(a.id, android("P")),
            &Save::Assignment(a.id, ios_own("B", "CLONE")),
        ]
    );
}

#[test]
fn a_saved_parked_assignment_is_loaded() {
    let a = workspace("a");
    let mut m: PoolMachine<Sess> = PoolMachine::new(DeviceCost::android(2048, 2), None);
    m.track(a.clone());
    let saved = Saved {
        assignments: [(a.id, android("P"))].into(),
        parked: [(a.id, ios_own("B", "CLONE"))].into(),
        ..Saved::default()
    };
    m.loaded(Ok(saved), None);
    assert_eq!(m.parked(a.id), Some(&ios_own("B", "CLONE")));
    assert_eq!(
        m.assignment_of(a.id, Platform::Ios),
        Some(&ios_own("B", "CLONE"))
    );
}

/// A (iOS, simulator CLONE open as DA) switched to Android at a cap of two while W, which
/// shares DA, waited: W's call holds DA now. C's Android device (DC) runs; D is of class B.
fn handed_over() -> (Machine, [WorkspaceRef; 4], PoolKey, Instant) {
    let ws = ["a", "w", "c", "d"].map(workspace);
    let mut m = pool(
        Limit::Manual(2),
        &[
            (&ws[0], ios_own("B", "CLONE")),
            (&ws[1], ios("B")),
            (&ws[2], android("P")),
            (&ws[3], ios("B")),
        ],
    );
    let t = Instant::now();
    let ka = own(&ws[0], Platform::Ios);
    assert_eq!(m.request_device(ws[0].id, t).unwrap().key, ka);
    open_running(&mut m, &ka, "DA", t);
    let kc = m.request_device(ws[2].id, t).unwrap().key;
    open_running(&mut m, &kc, "DC", t);
    let for_w = queued(m.request_device(ws[1].id, t).map(|s| s.lease));
    m.release_lease(&ka, ws[0].id, t);
    m.take_commands();
    done(m.switch_platform(ws[0].id, android("P"), SwitchBy::Agent, t));
    assert_eq!(grants(&m.take_commands()), [(for_w, ws[1].id, true)]);
    (m, ws, ka, t)
}

#[test]
fn another_base_cannot_drop_a_parked_device_a_call_uses() {
    let (mut m, [a, w, _, d], ka, t) = handed_over();
    let for_d = queued(m.request_device(d.id, t).map(|s| s.lease));
    m.take_commands();
    let refused: Result<(), String> =
        Err("DA is in use by a call of W; change it when that call ends".into());
    assert_eq!(
        m.switch_platform(a.id, ios("C"), SwitchBy::Agent, t)
            .map(|_| ()),
        refused
    );
    assert_eq!(
        m.assign(a.id, Platform::Ios, "C".into(), t).map(|_| ()),
        refused
    );
    assert!(m.take_commands().is_empty(), "nothing changed");
    assert_eq!(m.assignment(a.id), Some(&android("P")));
    // W's call ends: it only reserves DA now, and loses it; D stops waiting for it.
    m.release_lease(&ka, w.id, t + secs(1));
    m.take_commands();
    let (_, replaced) = done(m.switch_platform(a.id, ios("C"), SwitchBy::Agent, t + secs(1)));
    let replaced = replaced.unwrap();
    assert_eq!(replaced.clone.as_deref(), Some("CLONE"));
    assert_eq!(replaced.shutdown.map(|job| job.key), Some(ka.clone()));
    let commands = m.take_commands();
    assert!(
        commands.iter().any(|c| matches!(
            c,
            Command::Revoked { holder, key } if *holder == w.id && *key == ka
        )),
        "{commands:?}"
    );
    assert!(
        commands.iter().any(|c| matches!(
            c,
            Command::FailLease { token, message } if *token == for_d
                && message == "DA was replaced: its workspace chose another base device"
        )),
        "{commands:?}"
    );
    assert!(grants(&commands).is_empty(), "{commands:?}");
    assert!(m.rows().iter().all(|r| r.key != ka));
    assert_eq!(m.assignment(a.id), Some(&ios("C")));
}

#[test]
fn a_switch_by_the_user_is_told_to_the_agent() {
    let a = workspace("a");
    let mut m = pool(Limit::Auto, &[(&a, ios("B"))]);
    let t = Instant::now();
    let told = |commands: &[Command<Sess>]| -> Vec<(WorkspaceId, Platform)> {
        commands
            .iter()
            .filter_map(|c| match c {
                Command::UserSwitched { ws, platform } => Some((*ws, *platform)),
                _ => None,
            })
            .collect()
    };
    done(m.switch_platform(a.id, android("P"), SwitchBy::Agent, t));
    assert!(
        told(&m.take_commands()).is_empty(),
        "the agent asked: it knows"
    );
    done(m.switch_platform(a.id, ios("B"), SwitchBy::User, t));
    assert_eq!(told(&m.take_commands()), [(a.id, Platform::Ios)]);
    assert!(matches!(
        m.switch_platform(a.id, ios("B"), SwitchBy::User, t),
        Ok(Switched::Same)
    ));
    assert!(told(&m.take_commands()).is_empty(), "nothing switched");
}

#[test]
fn two_switches_resolved_together_apply_in_turn() {
    let a = workspace("a");
    let mut m = pool(Limit::Auto, &[(&a, ios_own("B", "CLONE"))]);
    let t = Instant::now();
    // Both resolved while iOS was attached: to Android, and to the iOS assignment.
    let back = m.assignment_of(a.id, Platform::Ios).cloned().unwrap();
    done(m.switch_platform(a.id, android("P"), SwitchBy::Agent, t));
    let (_, replaced) = done(m.switch_platform(a.id, back, SwitchBy::Agent, t));
    assert!(replaced.is_none(), "the same base keeps its simulator");
    assert_eq!(m.assignment(a.id), Some(&ios_own("B", "CLONE")));
    assert_eq!(m.parked(a.id), Some(&android("P")));
}

#[test]
fn loading_keeps_what_is_known_and_drops_a_parked_assignment_of_the_active_platform() {
    let ws = workspaces(&["a", "b", "c"]);
    let [a, b, c] = [&ws[0], &ws[1], &ws[2]];
    let mut m: PoolMachine<Sess> = PoolMachine::new(DeviceCost::android(2048, 2), None);
    for w in &ws {
        m.track(w.clone());
    }
    // Known before the settings were read.
    m.resolved(a.id, ios("R"));
    m.adopt(c.id, Some(ios("B")), Some(android("M")));
    m.take_commands();
    let saved = Saved {
        assignments: [(a.id, android("P")), (b.id, android("P")), (c.id, ios("B"))].into(),
        parked: [
            (a.id, ios("OLD")),
            (b.id, android("Q")),
            (c.id, android("S")),
        ]
        .into(),
        ..Saved::default()
    };
    m.loaded(Ok(saved), None);
    let commands = m.take_commands();
    let unparked: HashSet<WorkspaceId> = saves(&commands)
        .into_iter()
        .filter_map(|s| match s {
            Save::Unparked(ws) => Some(*ws),
            _ => None,
        })
        .collect();
    assert_eq!(unparked, HashSet::from([a.id, b.id]));
    assert_eq!(m.assignment(a.id), Some(&ios("R")));
    assert_eq!(m.parked(a.id), None);
    assert_eq!(m.assignment(b.id), Some(&android("P")));
    assert_eq!(m.parked(b.id), None);
    assert_eq!(m.parked(c.id), Some(&android("M")));
}
