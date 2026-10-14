use super::*;
use hangar_devices::capacity::Pressure;

fn slot(key: u32, booted: bool, busy: bool, focused: bool, last_used: u64) -> Slot<u32> {
    Slot {
        key,
        booted,
        busy,
        focused,
        last_used,
    }
}

#[test]
fn assignment_round_trips_and_tolerates_old_fields() {
    let a = Assignment {
        own: Some("UDID-OWN".into()),
        ..Assignment::new(Platform::Ios, "UDID-BASE")
    };
    let json = serde_json::to_string(&a).unwrap();
    assert_eq!(
        json,
        r#"{"platform":"ios","base":"UDID-BASE","own":"UDID-OWN","serial":null}"#
    );
    assert_eq!(serde_json::from_str::<Assignment>(&json).unwrap(), a);
    let minimal: Assignment =
        serde_json::from_str(r#"{"platform":"android","base":"Pixel_10_Pro"}"#).unwrap();
    assert_eq!(minimal, Assignment::new(Platform::Android, "Pixel_10_Pro"));
}

#[test]
fn limit_serializes_and_clamps() {
    assert_eq!(serde_json::to_string(&Limit::Auto).unwrap(), r#""auto""#);
    assert_eq!(
        serde_json::to_string(&Limit::Manual(3)).unwrap(),
        r#"{"manual":3}"#
    );
    assert_eq!(
        serde_json::from_str::<Limit>(r#"{"manual":3}"#).unwrap(),
        Limit::Manual(3)
    );
    assert_eq!(Limit::Auto.effective(5), 5);
    assert_eq!(Limit::Auto.effective(0), 1);
    assert_eq!(Limit::Manual(12).effective(2), MAX_SLOTS);
    assert_eq!(Limit::Manual(3).effective(8), 3);
    assert!(Limit::Manual(3).exceeds(2));
    assert!(!Limit::Manual(2).exceeds(2));
    assert!(!Limit::Auto.exceeds(1));
    assert_eq!(Limit::parse("auto"), Some(Limit::Auto));
    assert_eq!(Limit::parse(" 1 "), Some(Limit::Manual(1)));
    assert_eq!(Limit::parse("9"), None);
    assert_eq!(Limit::parse("0"), None);
}

#[test]
fn lru_idle_skips_busy_focused_and_off() {
    let slots = [
        slot(1, true, true, false, 1),
        slot(2, true, false, true, 2),
        slot(3, false, false, false, 0),
        slot(4, true, false, false, 9),
        slot(5, true, false, false, 4),
    ];
    assert_eq!(lru_idle(&slots), Some(5));
    assert_eq!(lru_idle(&slots[..3]), None);
}

#[test]
fn capacity_free_evict_or_full() {
    let slots = [
        slot(1, true, false, false, 3),
        slot(2, true, true, false, 1),
    ];
    assert_eq!(capacity(&slots, 3), Capacity::Free);
    assert_eq!(capacity(&slots, 2), Capacity::Evict(1));
    let busy = [slot(1, true, true, false, 3), slot(2, true, false, true, 1)];
    assert_eq!(capacity(&busy, 2), Capacity::Full);
    // Devices that are off do not take a slot.
    let off = [slot(1, false, false, false, 0)];
    assert_eq!(capacity(&off, 1), Capacity::Free);
}

#[test]
fn single_device_switching_waits_for_the_other_platform_to_go_idle() {
    // Limit 1: the iOS base device is booted and running a build (busy).
    let ios_busy = [slot(1, true, true, false, 5)];
    assert_eq!(capacity(&ios_busy, 1), Capacity::Full);
    // The build finished: the Android workspace may now take the only slot.
    let ios_idle = [slot(1, true, false, false, 5)];
    assert_eq!(capacity(&ios_idle, 1), Capacity::Evict(1));
}

#[test]
fn lowering_the_limit_shuts_down_idle_extras_only() {
    let slots = [
        slot(1, true, false, false, 5),
        slot(2, true, true, false, 1),
        slot(3, true, false, false, 2),
        slot(4, true, false, true, 0),
    ];
    assert_eq!(over_limit(&slots, 4), Vec::<u32>::new());
    assert_eq!(over_limit(&slots, 2), [3, 1]);
    // Three over, but only two are idle: the busy and focused ones stay for now.
    assert_eq!(over_limit(&slots, 1), [3, 1]);
}

fn host(available_ram_mb: u64, pressure: Pressure) -> HostResources {
    HostResources {
        total_ram_mb: 16384,
        available_ram_mb,
        perf_cores: 4,
        efficiency_cores: 4,
        pressure,
    }
}

#[test]
fn guard_refuses_under_pressure_or_low_memory() {
    let cost = DeviceCost::ios_simulator();
    assert_eq!(guard(Some(&host(8000, Pressure::Normal)), &cost), Ok(()));
    assert_eq!(
        guard(Some(&host(8000, Pressure::Warn)), &cost),
        Err(BootRefusal::Pressure(Pressure::Warn))
    );
    assert!(matches!(
        guard(Some(&host(3000, Pressure::Normal)), &cost),
        Err(BootRefusal::LowMemory { .. })
    ));
    assert_eq!(guard(None, &cost), Ok(()));
    assert_eq!(
        host_summary(&host(0, Pressure::Normal)),
        "16 GB RAM, 4 P + 4 E cores"
    );
}

#[test]
fn lease_is_fifo_and_reentrant() {
    let t = Instant::now();
    let mut q: LeaseQueue<&str, u32> = LeaseQueue::default();
    assert!(q.is_idle());
    assert_eq!(q.acquire("a", t, 1), Acquire::Granted(1));
    // Same workspace: shares the lease.
    assert_eq!(q.acquire("a", t, 2), Acquire::Granted(2));
    assert_eq!(q.acquire("b", t, 3), Acquire::Queued { position: 0 });
    assert_eq!(q.acquire("c", t, 4), Acquire::Queued { position: 1 });
    assert_eq!(q.acquire("b", t, 5), Acquire::Queued { position: 2 });
    assert!(q.is_waiting(&"b"));
    assert_eq!(q.release(&"a"), Vec::<u32>::new());
    // `b` was first in line: both of its waiters are granted together, `c` keeps waiting.
    assert_eq!(q.release(&"a"), [3, 5]);
    assert_eq!(q.holder(), Some(&"b"));
    assert_eq!(q.waiting_len(), 1);
    // Releasing what is not held changes nothing.
    assert_eq!(q.release(&"c"), Vec::<u32>::new());
    assert_eq!(q.release(&"b"), Vec::<u32>::new());
    assert_eq!(q.release(&"b"), [4]);
    assert_eq!(q.holder(), Some(&"c"));
    assert_eq!(q.release(&"c"), Vec::<u32>::new());
    assert!(q.is_idle());
}

#[test]
fn a_free_lease_is_not_taken_past_waiters() {
    let t = Instant::now();
    let mut q: LeaseQueue<&str, u32> = LeaseQueue::default();
    q.acquire("a", t, 1);
    q.acquire("b", t, 2);
    // `a` gives up its hold while `b` waits: `b` gets it, a newcomer queues behind.
    assert_eq!(q.release(&"a"), [2]);
    assert_eq!(q.acquire("c", t, 3), Acquire::Queued { position: 0 });
}

#[test]
fn lease_waiters_time_out_and_cancel() {
    let t = Instant::now();
    let mut q: LeaseQueue<&str, u32> = LeaseQueue::default();
    q.acquire("a", t, 1);
    q.acquire("b", t, 2);
    q.acquire("c", t + Duration::from_secs(5), 3);
    q.acquire("d", t + Duration::from_secs(6), 4);
    let expired = q.expire(t + Duration::from_secs(10), Duration::from_secs(10));
    assert_eq!(expired, [("b", 2)]);
    assert_eq!(q.cancel(&"d"), [4]);
    assert_eq!(q.waiting_len(), 1);
    assert_eq!(q.release(&"a"), [3]);
}
