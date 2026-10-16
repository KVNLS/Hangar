//! Pool decisions as pure functions over plain data, so they are tested without devices: the
//! saved assignment and limit, which device to shut down at the cap or under memory pressure,
//! and the per-device lease queue with its reservation rules.

use std::collections::VecDeque;
use std::time::{Duration, Instant};

use hangar_core::device::capacity::{BootRefusal, DeviceCost, HostResources, MAX_SLOTS};
use serde::{Deserialize, Serialize};

use hangar_core::device::Platform;

/// How long a run or tool call waits in a device's lease queue, after it queued or last polled,
/// before giving up.
pub const LEASE_TIMEOUT: Duration = Duration::from_secs(10 * 60);

/// A workspace keeps the device it used between its calls: with no call in flight, its
/// reservation ends this long after its last activity, and the next in line gets the device.
pub const RESERVATION_IDLE: Duration = Duration::from_secs(90);
/// How long a workspace may hold a device others wait for before it is warned, and
/// [`REVOKE_GRACE`] later loses it.
pub const MAX_HOLD: Duration = Duration::from_secs(5 * 60);
/// Between the warning and the revocation of a reservation held past [`MAX_HOLD`]. A call in
/// flight is never cut: the revocation waits for it to end.
pub const REVOKE_GRACE: Duration = Duration::from_secs(60);

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
        Some(host) => hangar_core::device::capacity::can_boot_another(host, cost),
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

/// Who holds a lease (its reservation), since when, and when it was last used.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Holder<W> {
    pub who: W,
    /// Re-entrant holds: one per run or tool call in flight. At zero the holder keeps an idle
    /// reservation until it ends ([`reservation_due`], [`LeaseQueue::end`]).
    pub holds: usize,
    /// When the lease was granted.
    pub since: Instant,
    /// The last acquire, release or touch of the holder.
    pub last_activity: Instant,
    /// When the holder was warned that its reservation will be revoked.
    pub warned: Option<Instant>,
    /// The reservation was ended while calls were in flight: the lease passes on when the last
    /// one is released, and new calls of the holder queue.
    pub ending: bool,
}

impl<W> Holder<W> {
    fn new(who: W, holds: usize, now: Instant) -> Self {
        Self {
            who,
            holds,
            since: now,
            last_activity: now,
            warned: None,
            ending: false,
        }
    }
}

#[derive(Debug)]
struct Waiter<W, T> {
    holder: W,
    /// When the waiter queued or last polled; it gives up [`LEASE_TIMEOUT`] after.
    last_seen: Instant,
    /// A call (released later) rather than a bare reservation.
    hold: bool,
    payload: T,
}

/// A waiter's payload handed back when its turn comes.
#[derive(Debug, PartialEq, Eq)]
pub struct Grant<T> {
    pub payload: T,
    /// Queued as a call, whose hold is released later; else as a bare reservation.
    pub hold: bool,
}

/// What to do with a reservation now ([`reservation_due`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Due {
    Keep,
    /// Idle for [`RESERVATION_IDLE`]: the next in line gets the device.
    Expire,
    /// Held past [`MAX_HOLD`] while others wait: tell the holder.
    Warn {
        ends_in: Duration,
    },
    /// Warned [`REVOKE_GRACE`] ago and no call in flight: take the device back.
    Revoke,
}

/// The reservation rules: idle expiry always, the hold limit only while someone waits.
pub fn reservation_due<W>(holder: &Holder<W>, waiting: bool, now: Instant) -> Due {
    let idle = now.saturating_duration_since(holder.last_activity);
    if holder.holds == 0 && idle >= RESERVATION_IDLE {
        return Due::Expire;
    }
    if !waiting || now.saturating_duration_since(holder.since) < MAX_HOLD {
        return Due::Keep;
    }
    match holder.warned {
        None => Due::Warn {
            ends_in: REVOKE_GRACE,
        },
        Some(at) if holder.holds == 0 && now.saturating_duration_since(at) >= REVOKE_GRACE => {
            Due::Revoke
        }
        Some(_) => Due::Keep,
    }
}

/// A device's lease: one holder at a time (re-entrant: several runs and calls of the same
/// workspace share it), others wait first come, first served. The holder keeps the lease
/// between its calls (a reservation) until [`LeaseQueue::end`].
#[derive(Debug)]
pub struct LeaseQueue<W, T> {
    holder: Option<Holder<W>>,
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
        self.holder.as_ref().map(|h| &h.who)
    }

    /// The holder with its hold count and times.
    pub fn holding(&self) -> Option<&Holder<W>> {
        self.holder.as_ref()
    }

    pub fn is_idle(&self) -> bool {
        self.holder.is_none() && self.waiting.is_empty()
    }

    pub fn is_waiting(&self, who: &W) -> bool {
        self.waiting.iter().any(|w| &w.holder == who)
    }

    /// Waiting tickets (a workspace may have several).
    pub fn waiting_len(&self) -> usize {
        self.waiting.len()
    }

    /// The waiting workspaces in turn order, each once.
    pub fn waiters(&self) -> Vec<&W> {
        let mut out: Vec<&W> = Vec::new();
        for w in &self.waiting {
            if !out.contains(&&w.holder) {
                out.push(&w.holder);
            }
        }
        out
    }

    /// Granted at once when free or already held by `who` (and nobody else waits for a free
    /// lease), else queued with `payload`, handed back once it is `who`'s turn. `hold`: a call,
    /// released later with [`Self::release`]; else a bare reservation, which takes no hold.
    pub fn acquire(&mut self, who: W, now: Instant, payload: T, hold: bool) -> Acquire<T> {
        match &mut self.holder {
            Some(h) if h.who == who && !h.ending => {
                h.holds += usize::from(hold);
                h.last_activity = now;
                Acquire::Granted(payload)
            }
            None if self.waiting.is_empty() => {
                self.holder = Some(Holder::new(who, usize::from(hold), now));
                Acquire::Granted(payload)
            }
            _ => {
                self.waiting.push_back(Waiter {
                    holder: who,
                    last_seen: now,
                    hold,
                    payload,
                });
                Acquire::Queued {
                    position: self.waiting.len() - 1,
                }
            }
        }
    }

    /// Like [`Self::acquire`], but ahead of everyone else in line (user take-over), with the
    /// other waiters of `who` moved up too. The holder is left alone; a holder `who` that was
    /// ending keeps the lease.
    pub fn jump(&mut self, who: W, now: Instant, payload: T, hold: bool) -> Acquire<T> {
        match &mut self.holder {
            Some(h) if h.who == who => {
                h.holds += usize::from(hold);
                h.last_activity = now;
                h.ending = false;
                return Acquire::Granted(payload);
            }
            None => return self.acquire(who, now, payload, hold),
            Some(_) => {}
        }
        let (mut first, rest): (VecDeque<_>, VecDeque<_>) =
            self.waiting.drain(..).partition(|w| w.holder == who);
        first.push_front(Waiter {
            holder: who,
            last_seen: now,
            hold,
            payload,
        });
        first.extend(rest);
        self.waiting = first;
        Acquire::Queued { position: 0 }
    }

    /// Releases one hold of `who`. The holder keeps the lease as an idle reservation, unless it
    /// was ending: then the next waiter gets it together with every other waiter of the same
    /// holder, and their payloads are returned.
    pub fn release(&mut self, who: &W, now: Instant) -> Vec<Grant<T>> {
        match &mut self.holder {
            Some(h) if &h.who == who => {
                h.holds = h.holds.saturating_sub(1);
                h.last_activity = now;
                if h.holds > 0 || !h.ending {
                    return Vec::new();
                }
                self.holder = None;
            }
            _ => return Vec::new(),
        }
        self.grant_next(now)
    }

    /// Ends the reservation of `who`: the lease passes on now, or when its last call in flight
    /// is released. Returns the payloads granted now.
    pub fn end(&mut self, who: &W, now: Instant) -> Vec<Grant<T>> {
        match &mut self.holder {
            Some(h) if &h.who == who => {
                if h.holds > 0 {
                    h.ending = true;
                    return Vec::new();
                }
                self.holder = None;
            }
            _ => return Vec::new(),
        }
        self.grant_next(now)
    }

    fn grant_next(&mut self, now: Instant) -> Vec<Grant<T>> {
        if self.holder.is_some() {
            return Vec::new();
        }
        let Some(next) = self.waiting.front().map(|w| w.holder.clone()) else {
            return Vec::new();
        };
        let (granted, rest): (VecDeque<_>, VecDeque<_>) =
            self.waiting.drain(..).partition(|w| w.holder == next);
        self.waiting = rest;
        let holds = granted.iter().filter(|w| w.hold).count();
        self.holder = Some(Holder::new(next, holds, now));
        granted
            .into_iter()
            .map(|w| Grant {
                payload: w.payload,
                hold: w.hold,
            })
            .collect()
    }

    /// Records activity of `who`: as the holder (its idle reservation lasts longer) and as a
    /// waiter ([`Self::poll`]). Whether it holds or waits here.
    pub fn touch(&mut self, who: &W, now: Instant) -> bool {
        let held = match &mut self.holder {
            Some(h) if &h.who == who => {
                h.last_activity = now;
                true
            }
            _ => false,
        };
        self.poll(who, now) || held
    }

    /// Records that the holder was warned of the revocation.
    pub fn warn(&mut self, now: Instant) {
        if let Some(h) = &mut self.holder {
            h.warned = Some(now);
        }
    }

    /// Nobody waits any more: a later wait warns again.
    pub fn clear_warning(&mut self) {
        if let Some(h) = &mut self.holder {
            h.warned = None;
        }
    }

    /// `who` is still there: its waiting tickets stay in line [`LEASE_TIMEOUT`] longer. Whether
    /// it waits here.
    pub fn poll(&mut self, who: &W, now: Instant) -> bool {
        let mut found = false;
        for w in self.waiting.iter_mut().filter(|w| &w.holder == who) {
            w.last_seen = now;
            found = true;
        }
        found
    }

    /// Removes waiters not seen (queued or polled) for `timeout`.
    pub fn expire(&mut self, now: Instant, timeout: Duration) -> Vec<(W, T)> {
        let (expired, rest): (VecDeque<_>, VecDeque<_>) = self
            .waiting
            .drain(..)
            .partition(|w| now.duration_since(w.last_seen) >= timeout);
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

    /// Removes every waiter (the device leaves the pool).
    pub fn take_waiting(&mut self) -> Vec<T> {
        self.waiting.drain(..).map(|w| w.payload).collect()
    }
}

#[cfg(test)]
mod tests;
