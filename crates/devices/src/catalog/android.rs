//! Android emulators and attached devices, through the SDK's `emulator` and `adb`.

use std::time::{Duration, Instant};

use hangar_core::device::catalog::{
    BackendReason, Conflict, Opened, Running, RunningInstance, Target,
};
use hangar_core::device::{DeviceCost, DeviceInfo, Platform};
use hangar_core::ports::{DeviceCatalog, PortError, PortResult};

use super::{base_choices, chain, forced_for, open_with_fallback, port, wrong_platform};
use crate::Device;
use crate::android::{self, AndroidSdk, AvdCost, Backend, BootOptions, discovery, emulator};

/// The AVDs, their running instances and attached devices.
pub struct Emulators;

impl DeviceCatalog for Emulators {
    type Device = Box<dyn Device>;

    fn platform(&self) -> Platform {
        Platform::Android
    }

    fn list(&self) -> PortResult<Vec<DeviceInfo>> {
        android::list().map_err(port)
    }

    fn boot_listed(&self, info: &DeviceInfo, progress: &dyn Fn(String)) -> PortResult<DeviceInfo> {
        android::boot_with_progress(&info.udid, &|stage| progress(stage.to_string())).map_err(port)
    }

    fn open(&self, info: DeviceInfo, forced: Option<&str>) -> PortResult<Opened<Self::Device>> {
        if let Some(name) = forced_for(Platform::Android, forced) {
            let backend: Backend = name.parse().map_err(PortError::new)?;
            let device = match backend {
                Backend::Grpc => android::open_grpc(&info),
                Backend::Adb => android::open_adb(&info),
            }
            .map_err(port)?;
            return Ok(Opened {
                device,
                reason: Some(BackendReason::Forced(format!(
                    "{name} backend forced by HANGAR_DEVICE_BACKEND"
                ))),
            });
        }
        if !Platform::Android.can_shutdown(&info) {
            // Physical devices only have the adb backend.
            return Ok(Opened {
                device: android::open_adb(&info).map_err(port)?,
                reason: None,
            });
        }
        let (device, fallback) = open_with_fallback(
            "grpc",
            || android::open_grpc(&info).map_err(port),
            "adb",
            || android::open_adb(&info).map_err(port),
        )?;
        Ok(Opened {
            device,
            reason: fallback.map(BackendReason::Fallback),
        })
    }

    fn probe(&self, target: &Target) -> PortResult<Option<Running>> {
        let Target::Android {
            avd,
            serial,
            shared,
        } = target
        else {
            return Err(wrong_platform("iOS devices on the Android catalog"));
        };
        let running = instances(avd)?;
        let mine = running
            .iter()
            .find(|r| serial.as_deref() == Some(r.serial().as_str()))
            .or_else(|| running.first().filter(|_| *shared));
        Ok(mine.map(|r| Running {
            info: openable(r.info.clone()),
            created: None,
        }))
    }

    fn create_if_missing(&self, target: &Target) -> PortResult<Option<String>> {
        match target {
            Target::IosOwn { .. } => Err(wrong_platform("simulators on the Android catalog")),
            _ => Ok(None),
        }
    }

    /// Android instances are always read-only: the emulator only runs several instances of one
    /// AVD when each is.
    fn boot(&self, target: &Target, progress: &dyn Fn(String)) -> PortResult<Running> {
        let Target::Android { avd, .. } = target else {
            return Err(wrong_platform("iOS devices on the Android catalog"));
        };
        let sdk = AndroidSdk::locate().map_err(port)?;
        if let Some(d) = emulator::conflicting_instance(&discovery::scan(), avd, true) {
            return Err(PortError::new(Conflict {
                avd: avd.clone(),
                serial: d.serial(),
            }));
        }
        let options = BootOptions {
            read_only: true,
            ..BootOptions::default()
        };
        let running = emulator::boot(&sdk, avd, &options, &|stage| progress(stage.to_string()))
            .map_err(port)?;
        Ok(Running {
            info: openable(running.info),
            created: None,
        })
    }

    fn shutdown(&self, id: &str) -> PortResult<()> {
        let sdk = AndroidSdk::locate().map_err(port)?;
        android::adb::Adb::new(sdk.adb)
            .emu(id, &["kill"])
            .map_err(|e| PortError::new(format!("adb -s {id} emu kill: {}", chain(&e))))
    }

    fn shutdown_if_running(&self, target: &Target) -> PortResult<bool> {
        let mut set = hangar_core::device::RunningSet::default();
        set.add(Platform::Android, self.running()?);
        let Some(id) = set.find(target) else {
            return Ok(false);
        };
        self.shutdown(&id)?;
        Ok(true)
    }

    fn running(&self) -> PortResult<Vec<RunningInstance>> {
        Ok(discovery::scan()
            .into_iter()
            .filter(|d| d.read_only)
            .map(|d| RunningInstance {
                id: d.serial(),
                base: Some(d.avd_id),
            })
            .collect())
    }

    fn clear_conflict(&self, conflict: &Conflict) -> PortResult<()> {
        self.shutdown(&conflict.serial)?;
        let deadline = Instant::now() + Duration::from_secs(30);
        while Instant::now() < deadline {
            let gone = !discovery::scan()
                .iter()
                .any(|d| d.serial() == conflict.serial);
            if gone {
                tracing::info!(target: "device_pool", serial = %conflict.serial, "conflicting emulator shut down");
                return Ok(());
            }
            std::thread::sleep(Duration::from_millis(250));
        }
        Err(PortError::new(format!(
            "{} still runs 30 s after asking it to shut down",
            conflict.serial
        )))
    }

    fn delete_clone(&self, _: &str) -> PortResult<()> {
        Err(wrong_platform("the pool makes no copies of AVDs"))
    }

    fn list_clones(&self) -> PortResult<Vec<DeviceInfo>> {
        Err(wrong_platform("the pool makes no copies of AVDs"))
    }

    fn cost(&self, base: Option<&str>) -> DeviceCost {
        let avd = match (base, android::avd::avd_home()) {
            (Some(avd), Some(home)) => android::avd::cost(&home, avd),
            _ => AvdCost::DEFAULT,
        };
        crate::capacity::android_cost(&avd)
    }

    fn bases(&self) -> PortResult<Vec<(String, String)>> {
        Ok(base_choices(Platform::Android, self.list()?))
    }
}

/// Android's `resolve` would also match the first running instance of the same AVD through
/// `device_type`; the serial alone identifies the instance.
pub(super) fn openable(info: DeviceInfo) -> DeviceInfo {
    DeviceInfo {
        device_type: None,
        ..info
    }
}

fn instances(avd: &str) -> PortResult<Vec<android::RunningEmulator>> {
    let sdk = AndroidSdk::locate().map_err(port)?;
    let mut running = android::list_running(&sdk).map_err(port)?;
    running.retain(|r| r.discovery.avd_id == avd && r.info.state == "Booted");
    Ok(running)
}
