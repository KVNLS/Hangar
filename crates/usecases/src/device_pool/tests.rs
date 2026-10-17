//! The pool on plain data: calls and I/O results in, commands out.

use std::time::{Duration, Instant};

use hangar_core::device::state::BOOTED;
use hangar_core::device::{DeviceCost, HostResources, Running, RunningInstance, RunningSet};

use super::*;

#[derive(Clone, Debug, PartialEq)]
struct Sess(DeviceInfo);

impl Session for Sess {
    fn info(&self) -> &DeviceInfo {
        &self.0
    }

    fn backend(&self) -> &'static str {
        "test"
    }
}

fn info(udid: &str, name: &str) -> DeviceInfo {
    DeviceInfo {
        udid: udid.into(),
        name: name.into(),
        runtime: "iOS 27.0".into(),
        state: BOOTED.into(),
        device_type: None,
        screen: None,
    }
}

fn host(pressure: Pressure) -> HostResources {
    HostResources {
        total_ram_mb: 48 * 1024,
        available_ram_mb: 40_000,
        perf_cores: 6,
        efficiency_cores: 12,
        pressure,
    }
}

fn workspace(slug: &str) -> WorkspaceRef {
    WorkspaceRef {
        id: WorkspaceId::new(),
        name: slug.to_uppercase(),
        project: "app".into(),
        slug: slug.into(),
        root: PathBuf::from("/tmp").join(slug),
    }
}

type Machine = PoolMachine<Sess>;

/// A pool with the host read (room for 8 simulators) and `assigned` loaded.
fn pool(limit: Limit, assigned: &[(&WorkspaceRef, Assignment)]) -> Machine {
    let mut m = Machine::new(DeviceCost::android(2048, 2), None);
    for (w, _) in assigned {
        m.track((*w).clone());
    }
    let saved = Saved {
        limit: Some(limit),
        assignments: assigned.iter().map(|(w, a)| (w.id, a.clone())).collect(),
        ..Saved::default()
    };
    m.loaded(Ok(saved), None);
    let commands = m.take_commands();
    assert!(matches!(
        commands.as_slice(),
        [
            Command::DetectHost { android_base: None },
            Command::ScanRunning
        ]
    ));
    m.host_detected(
        Ok(host(Pressure::Normal)),
        DeviceCost::android(2048, 2),
        Instant::now(),
    );
    assert!(m.take_commands().is_empty(), "48 GB needs no alert");
    m
}

fn ios(base: &str) -> Assignment {
    Assignment::new(Platform::Ios, base)
}

fn steps(commands: &[Command<Sess>]) -> Vec<&BringUp> {
    commands
        .iter()
        .filter_map(|c| match c {
            Command::BringUp(b) => Some(b),
            _ => None,
        })
        .collect()
}

/// The one bring-up step among `commands`.
fn step(commands: &[Command<Sess>]) -> BringUp {
    let found = steps(commands);
    assert_eq!(found.len(), 1, "{commands:?}");
    found[0].clone()
}

fn replies(commands: &[Command<Sess>]) -> Vec<(Token, Result<Sess, OpenError>)> {
    commands
        .iter()
        .filter_map(|c| match c {
            Command::ReplyOpen { token, result } => Some((*token, result.clone())),
            _ => None,
        })
        .collect()
}

fn wait(start: OpenStart<Sess>) -> Token {
    match start {
        OpenStart::Wait(token) => token,
        OpenStart::Ready(r) => panic!("ready at once: {r:?}"),
    }
}

/// Opens a base simulator that is not running: probe, claim, guard, boot, open.
fn open_base(m: &mut Machine, key: &PoolKey, udid: &str, now: Instant) -> Token {
    let token = wait(m.open(key, now));
    let b = step(&m.take_commands());
    assert_eq!(b.step, Step::Probe(Target::Ios { udid: udid.into() }));
    m.step_done(key, b.attempt, StepDone::Probed(Ok(None)), now);
    let b = step(&m.take_commands());
    assert_eq!(b.step, Step::CheckHost);
    m.step_done(
        key,
        b.attempt,
        StepDone::HostChecked(Ok(host(Pressure::Normal))),
        now,
    );
    let b = step(&m.take_commands());
    assert_eq!(b.step, Step::Boot(Target::Ios { udid: udid.into() }));
    finish_boot(m, key, b.attempt, udid, now);
    token
}

fn finish_boot(m: &mut Machine, key: &PoolKey, attempt: u64, udid: &str, now: Instant) {
    let running = Running {
        info: info(udid, udid),
        created: None,
    };
    m.step_done(key, attempt, StepDone::Booted(Ok(running)), now);
    let b = step(&m.take_commands());
    assert!(matches!(b.step, Step::Open { .. }), "{b:?}");
    m.step_done(
        key,
        attempt,
        StepDone::Opened(Ok((Sess(info(udid, udid)), None))),
        now,
    );
}

fn base_key(id: &str) -> PoolKey {
    PoolKey::Base(DeviceKey {
        platform: Platform::Ios,
        id: id.into(),
    })
}

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
fn leases_queue_hand_over_and_time_out() {
    let (a, b, c) = (workspace("a"), workspace("b"), workspace("c"));
    let assigned = [(&a, ios("X")), (&b, ios("X")), (&c, ios("X"))];
    let mut m = pool(Limit::Manual(1), &assigned);
    let now = Instant::now();
    let x = base_key("X");
    assert_eq!(m.request_lease(&x, a.id, now), Ok(LeaseStart::Granted));
    let Ok(LeaseStart::Queued(for_b)) = m.request_lease(&x, b.id, now) else {
        panic!("b should wait");
    };
    let Ok(LeaseStart::Queued(for_c)) = m.request_lease(&x, c.id, now) else {
        panic!("c should wait");
    };
    assert_eq!(m.held_by_other(b.id).map(|(ws, _)| ws), Some(a.id));
    m.release_lease(&x, a.id, now);
    let commands = m.take_commands();
    assert!(matches!(
        commands.as_slice(),
        [Command::GrantLease { token, holder, .. }] if *token == for_b && *holder == b.id
    ));
    m.monitor(now + LEASE_TIMEOUT, Ok(Pressure::Normal), None);
    let commands = m.take_commands();
    assert!(matches!(
        commands.as_slice(),
        [Command::FailLease { token, message }]
            if *token == for_c && message == "waited 10 minutes for X without getting it"
    ));
    assert!(!m.is_waiting(c.id));
}

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
fn a_new_base_replaces_the_own_device() {
    let w = workspace("a");
    let mut a = ios("OLD");
    a.own = Some("CLONE".into());
    let mut m = pool(Limit::Auto, &[(&w, a)]);
    let now = Instant::now();
    let key = PoolKey::Own(w.id, Platform::Ios);
    let token = wait(m.open(&key, now));
    let probe = step(&m.take_commands());
    let replaced = m
        .assign(w.id, Platform::Ios, "NEW".into(), Instant::now())
        .unwrap()
        .unwrap();
    assert!(replaced.shutdown.is_none(), "not running yet");
    assert_eq!(replaced.clone.as_deref(), Some("CLONE"));
    let commands = m.take_commands();
    assert_eq!(
        replies(&commands),
        [(token, Err(OpenError::Failed(STOPPED.into())))]
    );
    assert!(commands.iter().any(|c| matches!(
        c,
        Command::Save(Save::LastTarget(k)) if k.id == "NEW"
    )));
    assert_eq!(m.assignment(w.id).map(|a| a.base.as_str()), Some("NEW"));
    // The old bring-up's results land nowhere.
    m.step_done(&key, probe.attempt, StepDone::Probed(Ok(None)), now);
    assert!(m.take_commands().is_empty());
    assert!(
        m.assign(w.id, Platform::Ios, "NEW".into(), Instant::now())
            .unwrap()
            .is_none()
    );
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
fn a_small_host_alerts_once() {
    let mut m = Machine::new(DeviceCost::android(2048, 2), None);
    m.loaded(Ok(Saved::default()), None);
    m.take_commands();
    let small = HostResources {
        total_ram_mb: 8 * 1024,
        ..host(Pressure::Normal)
    };
    m.host_detected(Ok(small), DeviceCost::android(2048, 2), Instant::now());
    let commands = m.take_commands();
    assert!(matches!(
        commands.as_slice(),
        [
            Command::Notice(NoticeLevel::Warning, n),
            Command::Save(Save::SingleDeviceAlertShown)
        ] if n.starts_with("This Mac (8 GB RAM, 6 P + 12 E cores) can run one iOS or Android")
    ));
    assert!(m.single_device(Platform::Ios));
    m.host_detected(Ok(small), DeviceCost::android(2048, 2), Instant::now());
    assert!(m.take_commands().is_empty());
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

mod sharing;
