//! Pool decisions as pure functions over plain data, so they are tested without devices: the
//! saved assignment and limit, which device to shut down at the cap or under memory pressure,
//! and the per-device lease queue.

use std::collections::VecDeque;
use std::time::{Duration, Instant};

use hangar_core::device::capacity::{BootRefusal, DeviceCost, HostResources, MAX_SLOTS};
use serde::{Deserialize, Serialize};

use hangar_core::device::Platform;

/// How long a run or tool call waits in a device's lease queue, after it queued or last polled,
/// before giving up.
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

/// Who holds a lease, since when, and when it was last used.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Holder<W> {
    pub who: W,
    /// Re-entrant holds: one per run or tool call in flight.
    pub holds: usize,
    /// When the lease was granted.
    pub since: Instant,
    /// The last acquire, release or poll of the holder.
    pub last_activity: Instant,
}

#[derive(Debug)]
struct Waiter<W, T> {
    holder: W,
    /// When the waiter queued or last polled; it gives up [`LEASE_TIMEOUT`] after.
    last_seen: Instant,
    payload: T,
}

/// A device's lease: one holder at a time (re-entrant: several runs and calls of the same
/// workspace share it), others wait first come, first served.
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
    /// lease), else queued with `payload`, handed back by [`Self::release`] once it is `who`'s turn.
    pub fn acquire(&mut self, who: W, now: Instant, payload: T) -> Acquire<T> {
        match &mut self.holder {
            Some(h) if h.who == who => {
                h.holds += 1;
                h.last_activity = now;
                Acquire::Granted(payload)
            }
            None if self.waiting.is_empty() => {
                self.holder = Some(Holder {
                    who,
                    holds: 1,
                    since: now,
                    last_activity: now,
                });
                Acquire::Granted(payload)
            }
            _ => {
                self.waiting.push_back(Waiter {
                    holder: who,
                    last_seen: now,
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
    pub fn release(&mut self, who: &W, now: Instant) -> Vec<T> {
        match &mut self.holder {
            Some(h) if &h.who == who => {
                h.holds = h.holds.saturating_sub(1);
                h.last_activity = now;
                if h.holds > 0 {
                    return Vec::new();
                }
                self.holder = None;
            }
            _ => return Vec::new(),
        }
        self.grant_next(now)
    }

    fn grant_next(&mut self, now: Instant) -> Vec<T> {
        if self.holder.is_some() {
            return Vec::new();
        }
        let Some(next) = self.waiting.front().map(|w| w.holder.clone()) else {
            return Vec::new();
        };
        let (granted, rest): (VecDeque<_>, VecDeque<_>) =
            self.waiting.drain(..).partition(|w| w.holder == next);
        self.waiting = rest;
        self.holder = Some(Holder {
            who: next,
            holds: granted.len(),
            since: now,
            last_activity: now,
        });
        granted.into_iter().map(|w| w.payload).collect()
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
