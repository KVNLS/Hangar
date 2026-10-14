//! The [`DeviceCatalog`] adapters: [`Simulators`] (iOS, `simctl` and the private backend) and
//! [`Emulators`] (Android, `emulator` and `adb`), plus [`SystemHost`] behind
//! [`hangar_core::ports::HostMonitor`].

mod android;
mod ios;

pub use android::Emulators;
pub use hangar_core::ports::{DeviceCatalog, HostMonitor};
pub use ios::Simulators;

use hangar_core::device::{DeviceInfo, HostResources, Platform, Pressure};
use hangar_core::ports::{PortError, PortResult};

use crate::Device;

/// The catalog of either platform, as the app holds it.
pub type Catalog = dyn DeviceCatalog<Device = Box<dyn Device>>;

/// The adapter for `platform`.
pub fn for_platform(platform: Platform) -> &'static Catalog {
    match platform {
        Platform::Ios => &Simulators,
        Platform::Android => &Emulators,
    }
}

/// This Mac's memory and cores ([`crate::capacity`]).
pub struct SystemHost;

impl HostMonitor for SystemHost {
    fn detect(&self) -> PortResult<HostResources> {
        crate::capacity::detect().map_err(PortError::new)
    }

    fn pressure(&self) -> PortResult<Pressure> {
        crate::capacity::pressure_with_override().map_err(PortError::new)
    }
}

/// The error and its sources, `: `-separated (as `anyhow` prints `{:#}`).
pub(crate) fn chain(err: &(dyn std::error::Error + 'static)) -> String {
    let mut out = err.to_string();
    let mut source = err.source();
    while let Some(s) = source {
        out.push_str(": ");
        out.push_str(&s.to_string());
        source = s.source();
    }
    out
}

fn port(err: crate::Error) -> PortError {
    PortError::new(err)
}

fn wrong_platform(what: &'static str) -> PortError {
    port(crate::Error::Unsupported(what))
}

/// Runs `primary`; if it fails, runs `fallback` and returns why the fallback was needed.
pub fn open_with_fallback<T>(
    primary_name: &str,
    primary: impl FnOnce() -> PortResult<T>,
    fallback_name: &str,
    fallback: impl FnOnce() -> PortResult<T>,
) -> PortResult<(T, Option<String>)> {
    match primary() {
        Ok(value) => Ok((value, None)),
        Err(primary_err) => {
            let reason = format!("{primary_name} backend failed: {}", chain(&primary_err));
            tracing::warn!("{reason}; trying {fallback_name}");
            match fallback() {
                Ok(value) => Ok((value, Some(reason))),
                Err(fallback_err) => Err(PortError::new(format!(
                    "{reason}; {fallback_name} fallback failed: {}",
                    chain(&fallback_err)
                ))),
            }
        }
    }
}

/// The forced backend (`HANGAR_DEVICE_BACKEND`) that applies to `platform`. The variable is
/// shared: another platform's backend name is ignored, an unknown name is kept so opening fails.
pub fn forced_for(platform: Platform, forced: Option<&str>) -> Option<&str> {
    let other: &[&str] = match platform {
        Platform::Ios => &["grpc", "adb"],
        Platform::Android => &["private", "simctl"],
    };
    forced.filter(|f| !other.contains(f))
}

/// Base devices of `platform` from its listing, running ones first: simulator UDIDs (not the
/// pool's own clones) or AVD ids (not physical devices).
fn base_choices(platform: Platform, devices: Vec<DeviceInfo>) -> Vec<(String, String)> {
    let mut out: Vec<(bool, String, String)> = Vec::new();
    for d in devices {
        if d.name.starts_with(hangar_core::device::CLONE_PREFIX) {
            continue;
        }
        let running = platform.is_ready(&d);
        let id = match platform {
            Platform::Ios => d.udid,
            Platform::Android if d.udid.starts_with("emulator-") => match d.device_type {
                Some(avd) => avd,
                None => continue,
            },
            Platform::Android if d.state == hangar_core::device::state::SHUTDOWN => d.udid,
            Platform::Android => continue,
        };
        if !out.iter().any(|(_, known, _)| *known == id) {
            out.push((running, id, format!("{} · {}", d.name, d.runtime)));
        }
    }
    out.sort_by_key(|(running, _, _)| !*running);
    out.into_iter().map(|(_, id, title)| (id, title)).collect()
}

#[cfg(test)]
mod tests;
