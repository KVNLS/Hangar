//! The cap: saved settings, what the host can run, and the one-time single-device alert.

use std::time::Instant;

use hangar_core::WorkspaceId;
use hangar_core::device::Platform;
use hangar_core::device::capacity::{self, DeviceCost, HostResources, Pressure};

use super::policy::{self, Limit};
use super::{Command, NoticeLevel, PoolMachine, Save, Saved, Session};

/// What a waiting call gets when single-device mode switches on or off.
const MODE_CHANGED: &str =
    "the number of devices at once changed while this call waited; call again";

impl<S: Session> PoolMachine<S> {
    /// The saved settings were read (or not); `limit_override` (dev hook) replaces the saved
    /// limit for this run without saving it. Queues host detection and a scan of what runs.
    pub fn loaded(&mut self, saved: Result<Saved, String>, limit_override: Option<Limit>) {
        match saved {
            Ok(saved) => {
                self.limit = limit_override.or(saved.limit).unwrap_or_default();
                self.bases = saved.bases;
                self.single_alert_shown = saved.alert_shown;
                self.last_target = saved.last_target;
                self.assignments.extend(saved.assignments);
            }
            Err(err) => {
                tracing::error!(target: "device_pool", "failed to read the device settings: {err}");
                self.limit = limit_override.unwrap_or_default();
            }
        }
        if limit_override.is_some() {
            tracing::info!(target: "device_pool", limit = ?self.limit, "limit forced by HANGAR_DEV_DEVICE_LIMIT");
        }
        self.detect_host();
        self.push(Command::ScanRunning);
    }

    fn detect_host(&mut self) {
        let android_base = self.bases.get(&Platform::Android).cloned();
        self.push(Command::DetectHost { android_base });
    }

    /// What [`Command::DetectHost`] found.
    pub fn host_detected(
        &mut self,
        host: Result<HostResources, String>,
        android_cost: DeviceCost,
        now: Instant,
    ) {
        let before = self.modes();
        self.android_cost = android_cost;
        match host {
            Ok(host) => {
                self.pressure = Some(host.pressure);
                self.host = Some(host);
                tracing::info!(
                    target: "device_pool",
                    host = %policy::host_summary(&host),
                    available_mb = host.available_ram_mb,
                    pressure = %host.pressure,
                    ios_slots = self.recommended(Platform::Ios),
                    android_slots = self.recommended(Platform::Android),
                    limit = ?self.limit,
                    "host capacity"
                );
            }
            Err(err) => {
                tracing::warn!(target: "device_pool", "cannot read host resources, one device at a time: {err}");
                self.host_error = Some(err);
            }
        }
        self.maybe_alert_single();
        self.rebind_mode_changes(before, now);
        self.enforce_limit();
    }

    /// Notifies once (ever) that Auto found room for only one device.
    fn maybe_alert_single(&mut self) {
        if self.single_alert_shown || self.limit != Limit::Auto {
            return;
        }
        let Some(message) = self.single_device_explanation() else {
            return;
        };
        self.single_alert_shown = true;
        self.notice(NoticeLevel::Warning, message);
        self.save(Save::SingleDeviceAlertShown);
    }

    /// Why Auto runs one device at a time (with the numbers), when it does.
    pub fn single_device_explanation(&self) -> Option<String> {
        if self.limit != Limit::Auto {
            return None;
        }
        let single: Vec<&str> = Platform::ALL
            .into_iter()
            .filter(|p| self.recommended(*p) == 1)
            .map(Platform::label)
            .collect();
        if single.is_empty() {
            return None;
        }
        let host = match &self.host {
            Some(host) => policy::host_summary(host),
            None => format!(
                "host resources unreadable ({})",
                self.host_error.as_deref().unwrap_or("unknown")
            ),
        };
        Some(format!(
            "This Mac ({host}) can run one {} device at a time next to the agents and builds, so \
             workspaces share one device and take turns. Change it in Settings › Devices › \
             Devices at once.",
            single.join(" or ")
        ))
    }

    /// Whether host detection finished (the cap is known).
    pub fn host_known(&self) -> bool {
        self.host.is_some() || self.host_error.is_some()
    }

    pub fn limit(&self) -> Limit {
        self.limit
    }

    pub fn host(&self) -> Option<&HostResources> {
        self.host.as_ref()
    }

    pub fn pressure(&self) -> Option<Pressure> {
        self.pressure
    }

    pub fn base(&self, platform: Platform) -> Option<&str> {
        self.bases.get(&platform).map(String::as_str)
    }

    pub(super) fn cost(&self, platform: Platform) -> DeviceCost {
        match platform {
            Platform::Ios => DeviceCost::ios_simulator(),
            Platform::Android => self.android_cost,
        }
    }

    /// What the host can run of this platform's devices (1 when it cannot be read).
    pub fn recommended(&self, platform: Platform) -> u32 {
        self.host
            .as_ref()
            .map_or(1, |h| capacity::recommended_slots(h, &self.cost(platform)))
    }

    /// The cap on booted devices when booting one of `platform`.
    pub fn limit_for(&self, platform: Platform) -> u32 {
        self.limit.effective(self.recommended(platform))
    }

    pub fn single_device(&self, platform: Platform) -> bool {
        self.limit_for(platform) == 1
    }

    pub fn set_limit(&mut self, limit: Limit, now: Instant) {
        if limit == self.limit {
            return;
        }
        tracing::info!(target: "device_pool", ?limit, "devices at once changed");
        let before = self.modes();
        self.limit = limit;
        self.save(Save::Limit(limit));
        self.rebind_mode_changes(before, now);
        self.enforce_limit();
    }

    /// Whether each platform runs one device at a time, in [`Platform::ALL`] order.
    fn modes(&self) -> Vec<bool> {
        Platform::ALL
            .into_iter()
            .map(|p| self.single_device(p))
            .collect()
    }

    /// The workspaces of a platform that switched into or out of single-device mode use
    /// another device now (the base device, or their own): they leave the one they use.
    fn rebind_mode_changes(&mut self, before: Vec<bool>, now: Instant) {
        let switched: Vec<Platform> = Platform::ALL
            .into_iter()
            .zip(before)
            .filter(|(p, was)| self.single_device(*p) != *was)
            .map(|(p, _)| p)
            .collect();
        if switched.is_empty() {
            return;
        }
        let moved: Vec<WorkspaceId> = self
            .bindings
            .keys()
            .filter(|ws| {
                self.assignments
                    .get(ws)
                    .is_some_and(|a| switched.contains(&a.platform))
            })
            .copied()
            .collect();
        for ws in moved {
            self.unbind(ws, now, MODE_CHANGED);
        }
    }

    /// Base device used for this platform's workspaces when the user picked none.
    pub fn set_platform_base(&mut self, platform: Platform, id: String) {
        tracing::info!(target: "device_pool", platform = platform.label(), %id, "base device set");
        self.bases.insert(platform, id.clone());
        self.save(Save::Base(platform, id));
        if platform == Platform::Android {
            self.detect_host();
        }
    }
}
