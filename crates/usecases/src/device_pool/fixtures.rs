//! Builders shared by the pool tests: calls and I/O results in, commands out.

use hangar_core::device::Running;
use hangar_core::device::state::BOOTED;

use super::*;

#[derive(Clone, Debug, PartialEq)]
pub(super) struct Sess(pub(super) DeviceInfo);

impl Session for Sess {
    fn info(&self) -> &DeviceInfo {
        &self.0
    }

    fn backend(&self) -> &'static str {
        "test"
    }
}

pub(super) fn info(udid: &str, name: &str) -> DeviceInfo {
    DeviceInfo {
        udid: udid.into(),
        name: name.into(),
        runtime: "iOS 27.0".into(),
        state: BOOTED.into(),
        device_type: None,
        screen: None,
    }
}

pub(super) fn host(pressure: Pressure) -> HostResources {
    HostResources {
        total_ram_mb: 48 * 1024,
        available_ram_mb: 40_000,
        perf_cores: 6,
        efficiency_cores: 12,
        pressure,
    }
}

pub(super) fn workspace(slug: &str) -> WorkspaceRef {
    WorkspaceRef {
        id: WorkspaceId::new(),
        name: slug.to_uppercase(),
        project: "app".into(),
        slug: slug.into(),
        root: PathBuf::from("/tmp").join(slug),
    }
}

pub(super) type Machine = PoolMachine<Sess>;

/// A pool with the host read (room for 8 simulators) and `assigned` loaded.
pub(super) fn pool(limit: Limit, assigned: &[(&WorkspaceRef, Assignment)]) -> Machine {
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

pub(super) fn ios(base: &str) -> Assignment {
    Assignment::new(Platform::Ios, base)
}

pub(super) fn steps(commands: &[Command<Sess>]) -> Vec<&BringUp> {
    commands
        .iter()
        .filter_map(|c| match c {
            Command::BringUp(b) => Some(b),
            _ => None,
        })
        .collect()
}

/// The one bring-up step among `commands`.
pub(super) fn step(commands: &[Command<Sess>]) -> BringUp {
    let found = steps(commands);
    assert_eq!(found.len(), 1, "{commands:?}");
    found[0].clone()
}

pub(super) fn replies(commands: &[Command<Sess>]) -> Vec<(Token, Result<Sess, OpenError>)> {
    commands
        .iter()
        .filter_map(|c| match c {
            Command::ReplyOpen { token, result } => Some((*token, result.clone())),
            _ => None,
        })
        .collect()
}

pub(super) fn wait(start: OpenStart<Sess>) -> Token {
    match start {
        OpenStart::Wait(token) => token,
        OpenStart::Ready(r) => panic!("ready at once: {r:?}"),
    }
}

/// Opens a base simulator that is not running: probe, claim, guard, boot, open.
pub(super) fn open_base(m: &mut Machine, key: &PoolKey, udid: &str, now: Instant) -> Token {
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

pub(super) fn finish_boot(m: &mut Machine, key: &PoolKey, attempt: u64, udid: &str, now: Instant) {
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

pub(super) fn base_key(id: &str) -> PoolKey {
    PoolKey::Base(DeviceKey {
        platform: Platform::Ios,
        id: id.into(),
    })
}

pub(super) const fn secs(n: u64) -> Duration {
    Duration::from_secs(n)
}

/// The lease grants among `commands`: token, holder, whether it carries a hold.
pub(super) fn grants(commands: &[Command<Sess>]) -> Vec<(Token, WorkspaceId, bool)> {
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

pub(super) fn failed(commands: &[Command<Sess>]) -> Vec<Token> {
    commands
        .iter()
        .filter_map(|c| match c {
            Command::FailLease { token, .. } => Some(*token),
            _ => None,
        })
        .collect()
}

pub(super) fn queued(start: Result<LeaseStart, String>) -> Token {
    match start {
        Ok(LeaseStart::Queued(token)) => token,
        other => panic!("not queued: {other:?}"),
    }
}

pub(super) fn tick(m: &mut Machine, now: Instant) -> Vec<Command<Sess>> {
    m.monitor(now, Ok(Pressure::Normal), None);
    m.take_commands()
}

/// Three workspaces on the base device X (single-device mode), X open.
pub(super) fn shared_base() -> (Machine, [WorkspaceRef; 3], PoolKey, Instant) {
    let ws = [workspace("a"), workspace("b"), workspace("c")];
    let assigned: Vec<(&WorkspaceRef, Assignment)> = ws.iter().map(|w| (w, ios("X"))).collect();
    let mut m = pool(Limit::Manual(1), &assigned);
    let now = Instant::now();
    let x = base_key("X");
    open_base(&mut m, &x, "X", now);
    m.take_commands();
    (m, ws, x, now)
}

/// Opens `key`, found already running as `udid`.
pub(super) fn open_running(m: &mut Machine, key: &PoolKey, udid: &str, now: Instant) {
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

pub(super) fn workspaces(names: &[&str]) -> Vec<WorkspaceRef> {
    names.iter().map(|n| workspace(n)).collect()
}

/// Every holder whose reservation is not ending is bound to the device it holds, and every
/// waiter to the device it waits for.
pub(super) fn assert_bound_to_held(m: &Machine) {
    for (key, d) in &m.devices {
        if let Some(h) = d.leases.holding().filter(|h| !h.ending) {
            assert_eq!(m.bindings.get(&h.who), Some(key), "holder of {key:?}");
        }
        for ws in d.leases.waiters() {
            assert_eq!(m.bindings.get(ws), Some(key), "waiter of {key:?}");
        }
    }
}

pub(super) fn android(base: &str) -> Assignment {
    Assignment::new(Platform::Android, base)
}

pub(super) fn ios_own(base: &str, own: &str) -> Assignment {
    let mut a = ios(base);
    a.own = Some(own.into());
    a
}

pub(super) fn own(ws: &WorkspaceRef, platform: Platform) -> PoolKey {
    PoolKey::Own(ws.id, platform)
}

/// The jobs of a switch that went through.
pub(super) fn done(
    switched: Result<Switched<Sess>, String>,
) -> (Option<ShutdownJob<Sess>>, Option<Replaced<Sess>>) {
    match switched {
        Ok(Switched::Done { shutdown, replaced }) => (shutdown, replaced),
        other => panic!("not switched: {other:?}"),
    }
}

pub(super) fn saves(commands: &[Command<Sess>]) -> Vec<&Save> {
    commands
        .iter()
        .filter_map(|c| match c {
            Command::Save(save) => Some(save),
            _ => None,
        })
        .collect()
}

pub(super) fn shut_down(commands: &[Command<Sess>], key: &PoolKey) -> bool {
    commands
        .iter()
        .any(|c| matches!(c, Command::Shutdown(job) if job.key == *key))
}
