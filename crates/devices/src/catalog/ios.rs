//! iOS simulators. Everything but naming needs macOS; elsewhere calls fail as unsupported.

use hangar_core::device::catalog::{
    BackendReason, CLONE_PREFIX, Conflict, Opened, Running, RunningInstance, RunningSet, Target,
};
use hangar_core::device::{DeviceCost, DeviceInfo, Platform, state};
use hangar_core::ports::{DeviceCatalog, PortError, PortResult};

use super::{base_choices, chain, port, wrong_platform};
use crate::Device;

/// The simulators `simctl` knows.
pub struct Simulators;

impl DeviceCatalog for Simulators {
    type Device = Box<dyn Device>;

    fn platform(&self) -> Platform {
        Platform::Ios
    }

    fn list(&self) -> PortResult<Vec<DeviceInfo>> {
        let mut devices = sys::list_all()?;
        // tvOS, watchOS and visionOS simulators have no phone-style input to drive.
        devices.retain(|d| d.runtime.starts_with("iOS"));
        sort_ios(&mut devices);
        Ok(devices)
    }

    fn boot_listed(&self, info: &DeviceInfo, progress: &dyn Fn(String)) -> PortResult<DeviceInfo> {
        // `simctl boot` reports nothing until it returns.
        progress("booting the simulator".to_owned());
        sys::boot(&info.udid)?;
        Ok(DeviceInfo {
            state: state::BOOTED.to_owned(),
            ..info.clone()
        })
    }

    fn open(&self, info: DeviceInfo, forced: Option<&str>) -> PortResult<Opened<Self::Device>> {
        sys::open(info, forced)
    }

    fn probe(&self, target: &Target) -> PortResult<Option<Running>> {
        match target {
            Target::IosOwn { udid: None, .. } => Ok(None),
            Target::IosOwn {
                udid: Some(udid), ..
            } => Ok(find(udid)?
                .filter(|info| info.state == state::BOOTED)
                .map(|info| Running {
                    info,
                    created: None,
                })),
            Target::Ios { udid } => {
                let info = info(udid)?;
                Ok((info.state == state::BOOTED).then_some(Running {
                    info,
                    created: None,
                }))
            }
            Target::Android { .. } => Err(wrong_platform("Android devices on the iOS catalog")),
        }
    }

    fn create_if_missing(&self, target: &Target) -> PortResult<Option<String>> {
        let Target::IosOwn { udid, base, name } = target else {
            return Ok(None);
        };
        if let Some(udid) = udid
            && find(udid)?.is_some()
        {
            return Ok(None);
        }
        sys::create(base, name).map(Some)
    }

    fn boot(&self, target: &Target, progress: &dyn Fn(String)) -> PortResult<Running> {
        match target {
            Target::Ios { udid } => {
                progress("booting the simulator".to_owned());
                sys::boot(udid)?;
                Ok(Running {
                    info: info(udid)?,
                    created: None,
                })
            }
            Target::IosOwn { udid, base, name } => {
                if let Some(udid) = udid
                    && find(udid)?.is_some()
                {
                    progress("booting the simulator".to_owned());
                    sys::boot(udid)?;
                    return Ok(Running {
                        info: info(udid)?,
                        created: None,
                    });
                }
                progress("creating the simulator".to_owned());
                let udid = sys::create(base, name)?;
                progress("booting the simulator".to_owned());
                if let Err(err) = sys::boot(&udid) {
                    // A clone that never booted is useless: do not leave it behind.
                    if let Err(e) = sys::delete(&udid) {
                        tracing::warn!(target: "device_pool", %udid, "could not delete the failed clone: {}", chain(&e));
                    }
                    return Err(err);
                }
                Ok(Running {
                    info: info(&udid)?,
                    created: Some(udid),
                })
            }
            Target::Android { .. } => Err(wrong_platform("Android devices on the iOS catalog")),
        }
    }

    fn shutdown(&self, id: &str) -> PortResult<()> {
        sys::shutdown(id)
    }

    fn shutdown_if_running(&self, target: &Target) -> PortResult<bool> {
        let running = self.running().unwrap_or_else(|err| {
            tracing::debug!(target: "device_pool", "simulators not listed: {}", chain(&err));
            Vec::new()
        });
        let mut set = RunningSet::default();
        set.add(Platform::Ios, running);
        let Some(id) = set.find(target) else {
            return Ok(false);
        };
        self.shutdown(&id)?;
        Ok(true)
    }

    fn running(&self) -> PortResult<Vec<RunningInstance>> {
        Ok(sys::booted()?
            .into_iter()
            .map(|id| RunningInstance { id, base: None })
            .collect())
    }

    fn clear_conflict(&self, _: &Conflict) -> PortResult<()> {
        Err(wrong_platform("emulator conflicts on iOS"))
    }

    fn delete_clone(&self, udid: &str) -> PortResult<()> {
        if let Err(err) = sys::shutdown(udid) {
            tracing::warn!(target: "device_pool", %udid, "shutdown before delete: {}", chain(&err));
        }
        sys::delete(udid)
    }

    fn list_clones(&self) -> PortResult<Vec<DeviceInfo>> {
        let mut all = sys::list_all()?;
        all.retain(|d| d.name.starts_with(CLONE_PREFIX));
        Ok(all)
    }

    fn cost(&self, _: Option<&str>) -> DeviceCost {
        DeviceCost::ios_simulator()
    }

    fn bases(&self) -> PortResult<Vec<(String, String)>> {
        Ok(base_choices(Platform::Ios, self.list()?))
    }
}

fn find(udid: &str) -> PortResult<Option<DeviceInfo>> {
    Ok(sys::list_all()?.into_iter().find(|d| d.udid == udid))
}

fn info(udid: &str) -> PortResult<DeviceInfo> {
    find(udid)?.ok_or_else(|| PortError::new(format!("simulator {udid} not found (deleted?)")))
}

/// Orders simulators for the picker: booted first, then newest runtime first; `simctl` order
/// is kept within a runtime (phones before tablets).
pub fn sort_ios(devices: &mut [DeviceInfo]) {
    devices.sort_by_key(|d| {
        (
            d.state != state::BOOTED,
            std::cmp::Reverse(runtime_version(&d.runtime)),
        )
    });
}

/// Numeric components of a runtime name (`iOS 26.5` -> `[26, 5]`).
fn runtime_version(runtime: &str) -> Vec<u32> {
    runtime
        .rsplit(' ')
        .next()
        .unwrap_or_default()
        .split('.')
        .map_while(|part| part.parse().ok())
        .collect()
}

#[cfg(target_os = "macos")]
mod sys {
    use super::*;
    use crate::catalog::{forced_for, open_with_fallback};
    use crate::ios::{self, Backend};

    pub(super) fn list_all() -> PortResult<Vec<DeviceInfo>> {
        crate::simctl::list_all().map_err(port)
    }

    pub(super) fn booted() -> PortResult<Vec<String>> {
        crate::simctl::booted_udids().map_err(port)
    }

    pub(super) fn boot(udid: &str) -> PortResult<()> {
        ios::boot(udid).map_err(port)
    }

    pub(super) fn shutdown(udid: &str) -> PortResult<()> {
        ios::shutdown(udid).map_err(port)
    }

    pub(super) fn create(base: &str, name: &str) -> PortResult<String> {
        let spec = crate::simctl::spec(base).map_err(port)?;
        let udid = crate::simctl::create(name, &spec.device_type, &spec.runtime).map_err(port)?;
        tracing::info!(target: "device_pool", %udid, %name, base, "simulator created");
        Ok(udid)
    }

    pub(super) fn delete(udid: &str) -> PortResult<()> {
        crate::simctl::delete(udid).map_err(port)?;
        tracing::info!(target: "device_pool", %udid, "simulator deleted");
        Ok(())
    }

    pub(super) fn open(
        info: DeviceInfo,
        forced: Option<&str>,
    ) -> PortResult<Opened<Box<dyn Device>>> {
        let open = |backend| ios::open(info.clone(), backend).map_err(port);
        if let Some(name) = forced_for(Platform::Ios, forced) {
            let backend: Backend = name.parse().map_err(PortError::new)?;
            return Ok(Opened {
                device: open(backend)?,
                reason: Some(BackendReason::Forced(format!(
                    "{name} backend forced by HANGAR_DEVICE_BACKEND"
                ))),
            });
        }
        let (device, fallback) = open_with_fallback(
            "private",
            || open(Backend::Private),
            "simctl",
            || open(Backend::Simctl),
        )?;
        Ok(Opened {
            device,
            reason: fallback.map(BackendReason::Fallback),
        })
    }
}

#[cfg(not(target_os = "macos"))]
mod sys {
    use super::*;

    fn unsupported<T>() -> PortResult<T> {
        Err(port(crate::Error::Unsupported("iOS simulators need macOS")))
    }

    pub(super) fn list_all() -> PortResult<Vec<DeviceInfo>> {
        unsupported()
    }

    pub(super) fn booted() -> PortResult<Vec<String>> {
        unsupported()
    }

    pub(super) fn boot(_: &str) -> PortResult<()> {
        unsupported()
    }

    pub(super) fn shutdown(_: &str) -> PortResult<()> {
        unsupported()
    }

    pub(super) fn create(_: &str, _: &str) -> PortResult<String> {
        unsupported()
    }

    pub(super) fn delete(_: &str) -> PortResult<()> {
        unsupported()
    }

    pub(super) fn open(_: DeviceInfo, _: Option<&str>) -> PortResult<Opened<Box<dyn Device>>> {
        unsupported()
    }
}
