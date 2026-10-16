use super::*;
use hangar_core::device::Pressure;

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

fn granted(payloads: &[u32]) -> Vec<Grant<u32>> {
    payloads
        .iter()
        .map(|&payload| Grant {
            payload,
            hold: true,
        })
        .collect()
}

fn none() -> Vec<Grant<u32>> {
    Vec::new()
}

#[test]
fn lease_is_fifo_and_reentrant() {
    let t = Instant::now();
    let mut q: LeaseQueue<&str, u32> = LeaseQueue::default();
    assert!(q.is_idle());
    assert_eq!(q.acquire("a", t, 1, true), Acquire::Granted(1));
    // Same workspace: shares the lease.
    assert_eq!(q.acquire("a", t, 2, true), Acquire::Granted(2));
    assert_eq!(q.acquire("b", t, 3, true), Acquire::Queued { position: 0 });
    assert_eq!(q.acquire("c", t, 4, true), Acquire::Queued { position: 1 });
    assert_eq!(q.acquire("b", t, 5, true), Acquire::Queued { position: 2 });
    assert!(q.is_waiting(&"b"));
    assert_eq!(q.release(&"a", t), none());
    // The last call ended: `a` keeps an idle reservation until it ends.
    assert_eq!(q.release(&"a", t), none());
    assert_eq!(q.holding().map(|h| (h.who, h.holds)), Some(("a", 0)));
    // `b` was first in line: both of its waiters are granted together, `c` keeps waiting.
    assert_eq!(q.end(&"a", t), granted(&[3, 5]));
    assert_eq!(q.holder(), Some(&"b"));
    assert_eq!(q.waiting_len(), 1);
    // Releasing or ending what is not held changes nothing.
    assert_eq!(q.release(&"c", t), none());
    assert_eq!(q.end(&"c", t), none());
    assert_eq!(q.release(&"b", t), none());
    assert_eq!(q.release(&"b", t), none());
    assert_eq!(q.end(&"b", t), granted(&[4]));
    assert_eq!(q.holder(), Some(&"c"));
    assert_eq!(q.release(&"c", t), none());
    assert_eq!(q.end(&"c", t), none());
    assert!(q.is_idle());
}

#[test]
fn a_free_lease_is_not_taken_past_waiters() {
    let t = Instant::now();
    let mut q: LeaseQueue<&str, u32> = LeaseQueue::default();
    q.acquire("a", t, 1, true);
    q.acquire("b", t, 2, true);
    // `a` gives up its reservation while `b` waits: `b` gets it, a newcomer queues behind.
    q.release(&"a", t);
    assert_eq!(q.end(&"a", t), granted(&[2]));
    assert_eq!(q.acquire("c", t, 3, true), Acquire::Queued { position: 0 });
}

#[test]
fn lease_waiters_time_out_and_cancel() {
    let t = Instant::now();
    let mut q: LeaseQueue<&str, u32> = LeaseQueue::default();
    q.acquire("a", t, 1, true);
    q.acquire("b", t, 2, true);
    q.acquire("c", t + Duration::from_secs(5), 3, true);
    q.acquire("d", t + Duration::from_secs(6), 4, true);
    let expired = q.expire(t + Duration::from_secs(10), Duration::from_secs(10));
    assert_eq!(expired, [("b", 2)]);
    assert_eq!(q.cancel(&"d"), [4]);
    assert_eq!(q.waiting_len(), 1);
    q.release(&"a", t);
    assert_eq!(q.end(&"a", t), granted(&[3]));
}

#[test]
fn the_holder_records_when_it_got_and_last_used_the_lease() {
    let t = Instant::now();
    let s = |n| t + Duration::from_secs(n);
    let mut q: LeaseQueue<&str, u32> = LeaseQueue::default();
    q.acquire("a", t, 1, true);
    q.acquire("a", s(3), 2, true);
    q.acquire("b", s(4), 3, true);
    q.acquire("c", s(5), 4, true);
    q.acquire("b", s(6), 5, true);
    let h = q.holding().unwrap();
    assert_eq!(
        (h.who, h.holds, h.since, h.last_activity),
        ("a", 2, t, s(3))
    );
    assert_eq!(q.waiters(), [&"b", &"c"]);
    q.release(&"a", s(7));
    assert_eq!(q.holding().map(|h| h.last_activity), Some(s(7)));
    assert!(q.touch(&"a", s(8)));
    assert_eq!(q.holding().map(|h| h.last_activity), Some(s(8)));
    q.release(&"a", s(9));
    assert_eq!(q.end(&"a", s(10)), granted(&[3, 5]));
    let h = q.holding().unwrap();
    assert_eq!(
        (h.who, h.holds, h.since, h.last_activity),
        ("b", 2, s(10), s(10))
    );
}

#[test]
fn a_polled_waiter_stays_in_line() {
    let t = Instant::now();
    let timeout = Duration::from_secs(10);
    let mut q: LeaseQueue<&str, u32> = LeaseQueue::default();
    q.acquire("a", t, 1, true);
    q.acquire("b", t, 2, true);
    q.acquire("c", t, 3, true);
    assert!(q.poll(&"b", t + Duration::from_secs(8)));
    assert!(
        !q.poll(&"a", t + Duration::from_secs(8)),
        "holders do not wait"
    );
    assert_eq!(q.expire(t + timeout, timeout), [("c", 3)]);
    assert!(q.expire(t + Duration::from_secs(17), timeout).is_empty());
    assert_eq!(q.expire(t + Duration::from_secs(18), timeout), [("b", 2)]);
}

#[test]
fn ending_with_calls_in_flight_waits_for_them() {
    let t = Instant::now();
    let mut q: LeaseQueue<&str, u32> = LeaseQueue::default();
    q.acquire("a", t, 1, true);
    q.acquire("b", t, 2, true);
    assert_eq!(q.end(&"a", t), none());
    assert!(q.holding().unwrap().ending);
    // A new call of the ending holder queues behind `b`.
    assert_eq!(q.acquire("a", t, 3, true), Acquire::Queued { position: 1 });
    assert_eq!(q.release(&"a", t), granted(&[2]));
    assert_eq!(q.waiters(), [&"a"]);
}

#[test]
fn a_bare_reservation_takes_no_hold() {
    let t = Instant::now();
    let mut q: LeaseQueue<&str, u32> = LeaseQueue::default();
    assert_eq!(q.acquire("a", t, 1, false), Acquire::Granted(1));
    assert_eq!(q.holding().map(|h| h.holds), Some(0));
    q.acquire("b", t, 2, false);
    q.acquire("b", t, 3, true);
    assert_eq!(
        q.end(&"a", t),
        [
            Grant {
                payload: 2,
                hold: false
            },
            Grant {
                payload: 3,
                hold: true
            }
        ]
    );
    assert_eq!(q.holding().map(|h| h.holds), Some(1));
}

#[test]
fn jumping_the_line_moves_the_workspace_first() {
    let t = Instant::now();
    let mut q: LeaseQueue<&str, u32> = LeaseQueue::default();
    q.acquire("a", t, 1, true);
    q.acquire("b", t, 2, true);
    q.acquire("c", t, 3, true);
    assert_eq!(q.jump("c", t, 4, false), Acquire::Queued { position: 0 });
    assert_eq!(q.waiters(), [&"c", &"b"]);
    assert_eq!(q.release(&"a", t), none());
    assert_eq!(
        q.end(&"a", t),
        [
            Grant {
                payload: 4,
                hold: false
            },
            Grant {
                payload: 3,
                hold: true
            }
        ]
    );
    assert_eq!(q.jump("c", t, 5, false), Acquire::Granted(5));
}

#[test]
fn reservations_expire_idle_and_are_revoked_after_the_hold_limit() {
    let t = Instant::now();
    let holder = |holds, last: Duration, warned: Option<Duration>| Holder {
        who: "a",
        holds,
        since: t,
        last_activity: t + last,
        warned: warned.map(|w| t + w),
        ending: false,
    };
    let at = |d: Duration| t + d;
    let secs = Duration::from_secs;
    // Idle expiry, with or without waiters; never with a call in flight.
    let idle = holder(0, secs(0), None);
    assert_eq!(reservation_due(&idle, false, at(secs(89))), Due::Keep);
    assert_eq!(
        reservation_due(&idle, false, at(RESERVATION_IDLE)),
        Due::Expire
    );
    let busy = holder(1, secs(0), None);
    assert_eq!(reservation_due(&busy, false, at(secs(3600))), Due::Keep);
    // The hold limit only applies while someone waits.
    let active = holder(0, MAX_HOLD, None);
    assert_eq!(reservation_due(&active, false, at(MAX_HOLD)), Due::Keep);
    assert_eq!(
        reservation_due(&active, true, at(MAX_HOLD)),
        Due::Warn {
            ends_in: REVOKE_GRACE
        }
    );
    let warned = holder(0, MAX_HOLD + secs(30), Some(MAX_HOLD));
    assert_eq!(
        reservation_due(&warned, true, at(MAX_HOLD + secs(59))),
        Due::Keep
    );
    assert_eq!(
        reservation_due(&warned, true, at(MAX_HOLD + REVOKE_GRACE)),
        Due::Revoke
    );
    // A call in flight is never cut.
    let calling = holder(1, MAX_HOLD, Some(MAX_HOLD));
    assert_eq!(
        reservation_due(&calling, true, at(MAX_HOLD + REVOKE_GRACE)),
        Due::Keep
    );
}
