//! Bringing a device up: probe, claim a slot within the cap, check the host, create the
//! workspace's own simulator, boot, open.

use std::time::Instant;

use hangar_core::WorkspaceId;
use hangar_core::device::capacity::HostResources;
use hangar_core::device::{BackendReason, Conflict, Platform, Running, Target};

use super::policy::{self, Capacity, LEASE_TIMEOUT, Slot};
use super::{
    BringUp, Command, OpenError, Opening, Phase, PoolKey, PoolMachine, SLOT_POLL, Session, State,
    Step, Token,
};

/// Result of [`PoolMachine::open`].
#[derive(Debug)]
pub enum OpenStart<S> {
    Ready(Result<S, OpenError>),
    /// Answered later by [`Command::ReplyOpen`] with this token.
    Wait(Token),
}

/// A failed boot: the message, and the instance in the way when that was the reason.
#[derive(Clone, Debug, PartialEq)]
pub struct BootFailed {
    pub message: String,
    pub conflict: Option<Conflict>,
}

/// The result of a [`Step`]. Error strings are the adapter's error with its sources.
#[derive(Debug)]
pub enum StepDone<S> {
    Probed(Result<Option<Running>, String>),
    Waited,
    HostChecked(Result<HostResources, String>),
    Created(Result<Option<String>, String>),
    Booted(Result<Running, BootFailed>),
    Opened(Result<(S, Option<BackendReason>), String>),
}

impl<S: Session> PoolMachine<S> {
    pub(super) fn slots(&self, except: Option<&PoolKey>) -> Vec<Slot<PoolKey>> {
        let focused = self.focused_key();
        self.devices
            .iter()
            .filter(|(k, _)| Some(*k) != except)
            .map(|(k, d)| Slot {
                key: k.clone(),
                // One shutting down already makes room.
                booted: d.booted() && !matches!(d.state, State::ShuttingDown),
                busy: d.busy(),
                focused: focused.as_ref() == Some(k),
                last_used: d.last_used,
            })
            .collect()
    }

    /// Opens the device `key` (booting it if needed), or joins the bring-up in progress.
    pub fn open(&mut self, key: &PoolKey, now: Instant) -> OpenStart<S> {
        let token = self.token();
        let Some(device) = self.entry(key) else {
            return OpenStart::Ready(Err(OpenError::Failed(
                "no device assigned to this workspace".into(),
            )));
        };
        match &mut device.state {
            State::Open { session, .. } => return OpenStart::Ready(Ok(session.clone())),
            State::Opening(opening) => {
                opening.waiters.push(token);
                return OpenStart::Wait(token);
            }
            State::ShuttingDown => {
                return OpenStart::Ready(Err(OpenError::Failed(format!(
                    "{} is shutting down; try again in a moment",
                    device.name
                ))));
            }
            State::Closed => {}
        }
        device.error = None;
        device.conflict = None;
        device.state = State::Opening(Opening {
            stage: "checking the device".to_owned(),
            started: now,
            waiters: vec![token],
            attempt: token,
            phase: Phase::Probing,
        });
        let target = device.target.clone();
        self.mark_used(key);
        self.bring_up(key, token, Step::Probe(target));
        OpenStart::Wait(token)
    }

    /// Like [`Self::open`], for a call of `ws` granted the lease on `key`. A grant can be taken
    /// back before its call opens (the bring-up it would join moved its waiters elsewhere,
    /// [`Self::reroute`]): then [`OpenError::Moved`], so that the call asks again rather than
    /// boot a device it does not hold.
    pub fn open_held(&mut self, key: &PoolKey, ws: WorkspaceId, now: Instant) -> OpenStart<S> {
        if self.holds(ws, key) {
            return self.open(key, now);
        }
        let name = self.name_of(key).unwrap_or("the device").to_owned();
        tracing::info!(target: "device_pool", workspace = %ws, ?key, "the call's lease was taken back before it opened");
        OpenStart::Ready(Err(OpenError::Moved {
            to: self.key_for(ws).unwrap_or_else(|| key.clone()),
            message: format!(
                "{name} was taken back from this call before it opened: request the device again"
            ),
        }))
    }

    /// Whether a call of `ws` holds the lease on `key`.
    pub fn holds(&self, ws: WorkspaceId, key: &PoolKey) -> bool {
        self.devices
            .get(key)
            .and_then(|d| d.leases.holding())
            .is_some_and(|h| h.who == ws && h.holds > 0)
    }

    fn bring_up(&mut self, key: &PoolKey, attempt: u64, step: Step) {
        self.push(Command::BringUp(BringUp {
            key: key.clone(),
            attempt,
            step,
        }));
    }

    /// The bring-up `attempt` of `key`, if it is still the one in progress.
    fn opening(&mut self, key: &PoolKey, attempt: u64) -> Option<&mut Opening> {
        match &mut self.devices.get_mut(key)?.state {
            State::Opening(o) if o.attempt == attempt => Some(o),
            _ => None,
        }
    }

    fn set_phase(&mut self, key: &PoolKey, attempt: u64, phase: Phase) {
        if let Some(o) = self.opening(key, attempt) {
            o.phase = phase;
        }
    }

    fn set_stage(&mut self, key: &PoolKey, attempt: u64, stage: &str) {
        if let Some(o) = self.opening(key, attempt) {
            o.stage = stage.to_owned();
        }
    }

    fn set_running(&mut self, key: &PoolKey, running: bool) {
        if let Some(d) = self.devices.get_mut(key) {
            d.running = running;
        }
    }

    /// A boot stage reported while the device boots.
    pub fn stage(&mut self, key: &PoolKey, attempt: u64, stage: String) {
        if let Some(o) = self.opening(key, attempt)
            && o.phase == Phase::Booting
        {
            o.stage = stage;
        }
    }

    /// The result of the bring-up step `attempt` of `key`. Stale results are ignored.
    pub fn step_done(&mut self, key: &PoolKey, attempt: u64, done: StepDone<S>, now: Instant) {
        let Some(phase) = self.opening(key, attempt).map(|o| o.phase.clone()) else {
            return;
        };
        match done {
            StepDone::Probed(Err(err))
            | StepDone::Created(Err(err))
            | StepDone::Opened(Err(err)) => self.finish_open(key, Err(OpenError::Failed(err))),
            StepDone::Probed(Ok(Some(running))) => {
                self.set_running(key, true);
                self.open_running(key, attempt, running);
            }
            StepDone::Probed(Ok(None)) => {
                self.set_phase(
                    key,
                    attempt,
                    Phase::Claiming {
                        since: now,
                        evicting: None,
                    },
                );
                self.claim(key, attempt, now);
            }
            StepDone::Waited => self.claim(key, attempt, now),
            StepDone::HostChecked(host) => self.checked(key, attempt, host, &phase),
            StepDone::Created(Ok(created)) => self.created(key, attempt, created),
            StepDone::Booted(Err(failed)) => {
                if let Some(d) = self.devices.get_mut(key) {
                    d.running = false;
                    d.conflict = failed.conflict;
                }
                self.finish_open(key, Err(OpenError::Failed(failed.message)));
            }
            StepDone::Booted(Ok(running)) => self.open_running(key, attempt, running),
            StepDone::Opened(Ok((session, reason))) => {
                let created = match phase {
                    Phase::Opening { created } => created,
                    _ => None,
                };
                self.finish_open(key, Ok((session, reason, created)));
            }
        }
    }

    fn open_running(&mut self, key: &PoolKey, attempt: u64, running: Running) {
        let Some(platform) = self.devices.get(key).map(|d| d.platform) else {
            return;
        };
        self.set_stage(key, attempt, "opening");
        self.set_phase(
            key,
            attempt,
            Phase::Opening {
                created: running.created,
            },
        );
        let forced = self.forced_backend.clone();
        self.bring_up(
            key,
            attempt,
            Step::Open {
                platform,
                info: running.info,
                forced,
            },
        );
    }

    /// Takes a slot within the cap, shutting an idle device down or waiting for one; while it
    /// waits, those who wait for this device move to a compatible running one when there is.
    pub(super) fn claim(&mut self, key: &PoolKey, attempt: u64, now: Instant) {
        loop {
            let Some(Phase::Claiming { since, .. }) =
                self.opening(key, attempt).map(|o| o.phase.clone())
            else {
                return;
            };
            let Some(platform) = self.devices.get(key).map(|d| d.platform) else {
                return;
            };
            let limit = self.limit_for(platform);
            match policy::capacity(&self.slots(Some(key)), limit) {
                Capacity::Free => {
                    self.waiting_slot.remove(key);
                    self.set_running(key, true);
                    let cost = self.cost(platform);
                    self.set_phase(key, attempt, Phase::CheckingHost { cost });
                    self.bring_up(key, attempt, Step::CheckHost);
                    return;
                }
                Capacity::Evict(victim) => {
                    self.set_stage(key, attempt, "shutting down an idle device");
                    let name = self.devices.get(&victim).map(|d| d.name.clone());
                    tracing::info!(target: "device_pool", ?victim, ?name, limit, for_key = ?key, "at the cap: shutting down the least recently used idle device");
                    if let Some(job) = self.shutdown(&victim) {
                        self.set_phase(
                            key,
                            attempt,
                            Phase::Claiming {
                                since,
                                evicting: Some(victim),
                            },
                        );
                        self.push(Command::Shutdown(job));
                        return;
                    }
                }
                Capacity::Full => {
                    if self.reroute(key, now) {
                        return;
                    }
                    if now.duration_since(since) > LEASE_TIMEOUT {
                        self.waiting_slot.remove(key);
                        self.finish_open(
                            key,
                            Err(OpenError::Failed(format!(
                                "all {limit} device slots stayed busy for {} minutes",
                                LEASE_TIMEOUT.as_secs() / 60
                            ))),
                        );
                        return;
                    }
                    self.set_stage(key, attempt, "waiting for a free device slot");
                    if self.waiting_slot.insert(key.clone()) {
                        tracing::info!(target: "device_pool", ?key, limit, "every device slot is busy: waiting");
                    }
                    self.bring_up(key, attempt, Step::Wait(SLOT_POLL));
                    return;
                }
            }
        }
    }

    /// The eviction a claim waited for is over: claim again, or after a pause if it failed.
    pub(super) fn evicted(&mut self, victim: &PoolKey, result: &Result<(), String>, now: Instant) {
        let waiting: Vec<(PoolKey, u64, Instant)> = self
            .devices
            .iter()
            .filter_map(|(k, d)| match &d.state {
                State::Opening(Opening {
                    attempt,
                    phase:
                        Phase::Claiming {
                            since,
                            evicting: Some(v),
                        },
                    ..
                }) if v == victim => Some((k.clone(), *attempt, *since)),
                _ => None,
            })
            .collect();
        for (key, attempt, since) in waiting {
            self.set_phase(
                &key,
                attempt,
                Phase::Claiming {
                    since,
                    evicting: None,
                },
            );
            match result {
                Ok(()) => self.claim(&key, attempt, now),
                Err(err) => {
                    tracing::warn!(target: "device_pool", "shutdown failed: {err}");
                    self.bring_up(&key, attempt, Step::Wait(SLOT_POLL));
                }
            }
        }
    }

    fn checked(
        &mut self,
        key: &PoolKey,
        attempt: u64,
        host: Result<HostResources, String>,
        phase: &Phase,
    ) {
        let (Phase::CheckingHost { cost }, Some(platform)) =
            (phase, self.devices.get(key).map(|d| d.platform))
        else {
            return;
        };
        let guard = match &host {
            Ok(host) => policy::guard(Some(host), cost),
            Err(err) => {
                tracing::warn!(target: "device_pool", "boot not checked against host resources: {err}");
                Ok(())
            }
        };
        if let Err(refusal) = guard {
            let numbers = host.as_ref().map(policy::host_summary).unwrap_or_default();
            tracing::warn!(target: "device_pool", ?key, %refusal, %numbers, "boot refused");
            self.set_running(key, false);
            self.finish_open(
                key,
                Err(OpenError::Refused(format!(
                    "Not booting another {} device: {refusal} ({numbers}; Devices at once \
                     is in Settings › Devices)",
                    platform.label()
                ))),
            );
            return;
        }
        tracing::info!(target: "device_pool", ?key, "booting");
        let Some(target) = self.devices.get(key).map(|d| d.target.clone()) else {
            return;
        };
        if matches!(target, Target::IosOwn { .. }) {
            self.set_stage(key, attempt, "creating the simulator");
            self.set_phase(key, attempt, Phase::Creating);
            self.bring_up(key, attempt, Step::Create(target));
        } else {
            self.set_phase(key, attempt, Phase::Booting);
            self.bring_up(key, attempt, Step::Boot(target));
        }
    }

    /// The workspace's simulator exists now; `created` when it was just made.
    fn created(&mut self, key: &PoolKey, attempt: u64, created: Option<String>) {
        let Some(device) = self.devices.get_mut(key) else {
            return;
        };
        if let (Some(udid), Target::IosOwn { udid: own, .. }) = (created, &mut device.target) {
            *own = Some(udid.clone());
            if let PoolKey::Own(ws, platform) = key {
                self.record_own(*ws, *platform, Some(udid.clone()), udid);
            }
        }
        let Some(target) = self.devices.get(key).map(|d| d.target.clone()) else {
            return;
        };
        self.set_phase(key, attempt, Phase::Booting);
        self.bring_up(key, attempt, Step::Boot(target));
    }

    #[allow(clippy::type_complexity)]
    fn finish_open(
        &mut self,
        key: &PoolKey,
        result: Result<(S, Option<BackendReason>, Option<String>), OpenError>,
    ) {
        self.waiting_slot.remove(key);
        let Some(device) = self.devices.get_mut(key) else {
            return;
        };
        let State::Opening(opening) = std::mem::replace(&mut device.state, State::Closed) else {
            return;
        };
        let opened = match result {
            Ok((session, reason, created)) => {
                tracing::info!(
                    target: "device_pool",
                    ?key,
                    device = %session.info().udid,
                    name = %session.info().name,
                    backend = session.backend(),
                    "device open"
                );
                device.running = true;
                device.name = session.info().name.clone();
                let id = session.info().udid.clone();
                match &mut device.target {
                    Target::IosOwn { udid, .. } => *udid = Some(id.clone()),
                    Target::Android { serial, .. } => *serial = Some(id.clone()),
                    Target::Ios { .. } => {}
                }
                device.state = State::Open {
                    session: session.clone(),
                    reason,
                };
                if let PoolKey::Own(ws, platform) = key {
                    self.record_own(*ws, *platform, created, id);
                }
                Ok(session)
            }
            Err(err) => {
                tracing::warn!(target: "device_pool", ?key, error = err.message(), "device not opened");
                device.error = Some(err.message().to_owned());
                Err(err)
            }
        };
        for token in opening.waiters {
            self.push(Command::ReplyOpen {
                token,
                result: opened.clone(),
            });
        }
        self.enforce_limit();
    }

    /// Saves what the workspace's own device of `platform` is now (created simulator, Android
    /// serial), in its active or parked assignment.
    fn record_own(
        &mut self,
        ws: WorkspaceId,
        platform: Platform,
        created: Option<String>,
        id: String,
    ) {
        self.edit_assignment(ws, platform, |a| match a.platform {
            Platform::Ios => {
                if let Some(udid) = created {
                    a.own = Some(udid);
                }
            }
            Platform::Android => {
                a.own = Some(a.base.clone());
                a.serial = Some(id);
            }
        });
    }
}
