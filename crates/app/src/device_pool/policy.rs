//! Pool decisions as pure functions over plain data, so they are tested without devices: the
//! saved assignment and limit, which device to shut down at the cap or under memory pressure,
//! and the per-device lease queue.

use std::collections::VecDeque;
use std::time::{Duration, Instant};

use hangar_devices::capacity::{BootRefusal, DeviceCost, HostResources, MAX_SLOTS};
use serde::{Deserialize, Serialize};

use crate::device_panel::Platform;

/// How long a run or tool call waits in a device's lease queue before giving up.
pub const LEASE_TIMEOUT: Duration = Duration::from_secs(10 * 60);

/// A workspace's device, saved under `device:<workspace id>`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Assignment {
    pub platform: Platform,
    /// The device it is made from: a simulator UDID or an AVD id.
    pub base: String,
    /// The workspace's own device once created: the simulator UDID it was created as (iOS), or
    /// the AVD id it runs read-only instances of (Android).
    #[serde(default)]
    pub own: Option<String>,
    /// adb serial of the last Android instance the pool booted for it.
    #[serde(default)]
    pub serial: Option<String>,
}

impl Assignment {
    pub fn new(platform: Platform, base: impl Into<String>) -> Self {
        Self {
            platform,
            base: base.into(),
            own: None,
            serial: None,
        }
    }
}

/// "Devices at once": detected from the host, or chosen in Settings.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Limit {
    #[default]
    Auto,
    Manual(u32),
}

impl Limit {
    /// The cap given what the host can run (`recommended`), between 1 and [`MAX_SLOTS`].
    pub fn effective(self, recommended: u32) -> u32 {
        match self {
            Self::Auto => recommended,
            Self::Manual(n) => n,
        }
        .clamp(1, MAX_SLOTS)
    }

    /// A manual value above what the host can run (shown as a warning).
    pub fn exceeds(self, recommended: u32) -> bool {
        matches!(self, Self::Manual(n) if n > recommended)
    }

    /// `HANGAR_DEV_DEVICE_LIMIT` and similar: `auto` or `1`..=`8`.
    pub fn parse(value: &str) -> Option<Self> {
        match value.trim() {
            "auto" => Some(Self::Auto),
            n => n
                .parse::<u32>()
                .ok()
                .filter(|n| (1..=MAX_SLOTS).contains(n))
                .map(Self::Manual),
        }
    }
}

/// One managed device as the policy sees it.
#[derive(Clone, Debug, PartialEq)]
pub struct Slot<K> {
    pub key: K,
    /// Running (or booting): it takes a slot.
    pub booted: bool,
    /// A lease is held or awaited, or a boot/open is in flight.
    pub busy: bool,
    /// Shown in the device panel for the focused workspace.
    pub focused: bool,
    /// Larger is more recent.
    pub last_used: u64,
}

impl<K> Slot<K> {
    fn idle(&self) -> bool {
        self.booted && !self.busy && !self.focused
    }
}

/// The least recently used booted device nobody is using or looking at.
pub fn lru_idle<K: Clone>(slots: &[Slot<K>]) -> Option<K> {
    slots
        .iter()
        .filter(|s| s.idle())
        .min_by_key(|s| s.last_used)
        .map(|s| s.key.clone())
}

/// What booting one more device needs at a cap of `limit`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Capacity<K> {
    /// A slot is free.
    Free,
    /// Shut this idle device down first.
    Evict(K),
    /// Every slot is in use; wait for one to go idle.
    Full,
}

/// `slots` should not include the device about to boot.
pub fn capacity<K: Clone>(slots: &[Slot<K>], limit: u32) -> Capacity<K> {
    let booted = slots.iter().filter(|s| s.booted).count();
    if booted < limit as usize {
        return Capacity::Free;
    }
    match lru_idle(slots) {
        Some(key) => Capacity::Evict(key),
        None => Capacity::Full,
    }
}

/// Idle devices to shut down now so that no more than `limit` stay booted, least recently used
/// first. Busy extras are left alone; call again once they go idle.
pub fn over_limit<K: Clone>(slots: &[Slot<K>], limit: u32) -> Vec<K> {
    let booted = slots.iter().filter(|s| s.booted).count();
    let extra = booted.saturating_sub(limit as usize);
    let mut idle: Vec<&Slot<K>> = slots.iter().filter(|s| s.idle()).collect();
    idle.sort_by_key(|s| s.last_used);
    idle.into_iter()
        .take(extra)
        .map(|s| s.key.clone())
        .collect()
}

/// Whether another device may boot now. Hosts whose resources cannot be read are not judged
/// (the caller logs why).
pub fn guard(host: Option<&HostResources>, cost: &DeviceCost) -> Result<(), BootRefusal> {
    match host {
        Some(host) => hangar_devices::capacity::can_boot_another(host, cost),
        None => Ok(()),
    }
}

/// "48 GB RAM, 6 P + 12 E cores".
pub fn host_summary(host: &HostResources) -> String {
    let gb = host.total_ram_mb.div_ceil(1024);
    if host.efficiency_cores > 0 {
        format!(
            "{gb} GB RAM, {} P + {} E cores",
            host.perf_cores, host.efficiency_cores
        )
    } else {
        format!("{gb} GB RAM, {} cores", host.perf_cores)
    }
}

/// Result of [`LeaseQueue::acquire`].
#[derive(Debug, PartialEq, Eq)]
pub enum Acquire<T> {
    /// The lease is held now; the payload comes back unused.
    Granted(T),
    /// Waiting behind others (0 = next).
    Queued { position: usize },
}

#[derive(Debug)]
struct Waiter<W, T> {
    holder: W,
    since: Instant,
    payload: T,
}

/// A device's lease: one holder at a time (re-entrant: several runs and calls of the same
/// workspace share it), others wait first come, first served.
#[derive(Debug)]
pub struct LeaseQueue<W, T> {
    holder: Option<(W, usize)>,
    waiting: VecDeque<Waiter<W, T>>,
}

impl<W, T> Default for LeaseQueue<W, T> {
    fn default() -> Self {
        Self {
            holder: None,
            waiting: VecDeque::new(),
        }
    }
}

impl<W: Clone + PartialEq, T> LeaseQueue<W, T> {
    pub fn holder(&self) -> Option<&W> {
        self.holder.as_ref().map(|(w, _)| w)
    }

    pub fn is_idle(&self) -> bool {
        self.holder.is_none() && self.waiting.is_empty()
    }

    pub fn is_waiting(&self, who: &W) -> bool {
        self.waiting.iter().any(|w| &w.holder == who)
    }

    pub fn waiting_len(&self) -> usize {
        self.waiting.len()
    }

    /// Granted at once when free or already held by `who` (and nobody else waits for a free
    /// lease), else queued with `payload`, handed back by [`Self::release`] once it is `who`'s turn.
    pub fn acquire(&mut self, who: W, now: Instant, payload: T) -> Acquire<T> {
        match &mut self.holder {
            Some((holder, count)) if *holder == who => {
                *count += 1;
                Acquire::Granted(payload)
            }
            None if self.waiting.is_empty() => {
                self.holder = Some((who, 1));
                Acquire::Granted(payload)
            }
            _ => {
                self.waiting.push_back(Waiter {
                    holder: who,
                    since: now,
                    payload,
                });
                Acquire::Queued {
                    position: self.waiting.len() - 1,
                }
            }
        }
    }

    /// Releases one hold of `who`. When the lease becomes free, the next waiter gets it together
    /// with every other waiter of the same holder; their payloads are returned.
    pub fn release(&mut self, who: &W) -> Vec<T> {
        match &mut self.holder {
            Some((holder, count)) if holder == who => {
                *count -= 1;
                if *count > 0 {
                    return Vec::new();
                }
                self.holder = None;
            }
            _ => return Vec::new(),
        }
        self.grant_next()
    }

    fn grant_next(&mut self) -> Vec<T> {
        if self.holder.is_some() {
            return Vec::new();
        }
        let Some(next) = self.waiting.front().map(|w| w.holder.clone()) else {
            return Vec::new();
        };
        let (granted, rest): (VecDeque<_>, VecDeque<_>) =
            self.waiting.drain(..).partition(|w| w.holder == next);
        self.waiting = rest;
        self.holder = Some((next, granted.len()));
        granted.into_iter().map(|w| w.payload).collect()
    }

    /// Removes waiters queued for longer than `timeout`.
    pub fn expire(&mut self, now: Instant, timeout: Duration) -> Vec<(W, T)> {
        let (expired, rest): (VecDeque<_>, VecDeque<_>) = self
            .waiting
            .drain(..)
            .partition(|w| now.duration_since(w.since) >= timeout);
        self.waiting = rest;
        expired.into_iter().map(|w| (w.holder, w.payload)).collect()
    }

    /// Removes every waiter of `who` (workspace archived or deleted).
    pub fn cancel(&mut self, who: &W) -> Vec<T> {
        let (cancelled, rest): (VecDeque<_>, VecDeque<_>) =
            self.waiting.drain(..).partition(|w| &w.holder == who);
        self.waiting = rest;
        cancelled.into_iter().map(|w| w.payload).collect()
    }
}

#[cfg(test)]
mod tests {
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
}
