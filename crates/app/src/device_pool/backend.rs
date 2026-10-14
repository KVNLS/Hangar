//! Blocking device operations of the pool (simctl, emulator, adb). Call them on the background
//! executor only.

use std::sync::Arc;

use anyhow::{Context as _, anyhow};
use hangar_devices::DeviceInfo;
use hangar_devices::capacity::DeviceCost;

use crate::device_panel::{BackendReason, Platform};
use crate::device_view::DeviceSession;

/// Prefix of the simulators the pool creates (recognisable and cleanable in Device Hub).
pub const CLONE_PREFIX: &str = "Hangar · ";

/// `Hangar · <project>/<slug>`.
pub fn clone_name(project: &str, slug: &str) -> String {
    format!("{CLONE_PREFIX}{project}/{slug}")
}

/// What to bring up for one pool device.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Target {
    /// A simulator by UDID.
    Ios { udid: String },
    /// A workspace's own simulator: `udid` once created; (re)created like `base`, named `name`,
    /// when it does not exist.
    IosOwn {
        udid: Option<String>,
        base: String,
        name: String,
    },
    /// The AVD's instance (any) or a new read-only one. `serial` is the instance booted for this
    /// device earlier, used when it still runs.
    Android {
        avd: String,
        serial: Option<String>,
        /// Shared base device: any running instance of the AVD will do.
        shared: bool,
    },
}

/// The base AVD runs outside the pool without `-read-only`, which keeps the emulator from
/// starting any other instance of it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Conflict {
    pub avd: String,
    pub serial: String,
}

impl std::fmt::Display for Conflict {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{} is already running as {} without -read-only, so Hangar cannot start another \
             instance of it. Shut it down (Device tab: Shut down and retry), then try again.",
            self.avd, self.serial
        )
    }
}

impl std::error::Error for Conflict {}

/// Shuts down the instance in the way of the pool and waits until it is gone.
pub fn clear_conflict(conflict: &Conflict) -> anyhow::Result<()> {
    shutdown(Platform::Android, &conflict.serial)?;
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
    while std::time::Instant::now() < deadline {
        let gone = !hangar_devices::android::discovery::scan()
            .iter()
            .any(|d| d.serial() == conflict.serial);
        if gone {
            tracing::info!(target: "device_pool", serial = %conflict.serial, "conflicting emulator shut down");
            return Ok(());
        }
        std::thread::sleep(std::time::Duration::from_millis(250));
    }
    Err(anyhow!(
        "{} still runs 30 s after asking it to shut down",
        conflict.serial
    ))
}

/// A running device found or brought up, ready to open.
pub struct Running {
    pub info: DeviceInfo,
    /// UDID of a simulator created for this device.
    pub created: Option<String>,
}

/// The device if it already runs (nothing is booted).
pub fn probe(target: &Target) -> anyhow::Result<Option<Running>> {
    match target {
        Target::IosOwn { udid: None, .. } => Ok(None),
        Target::IosOwn {
            udid: Some(udid), ..
        } => Ok(ios_find(udid)?
            .filter(|info| info.state == "Booted")
            .map(|info| Running {
                info,
                created: None,
            })),
        Target::Ios { udid } => {
            let info = ios_info(udid)?;
            Ok((info.state == "Booted").then_some(Running {
                info,
                created: None,
            }))
        }
        Target::Android {
            avd,
            serial,
            shared,
        } => {
            let running = android_instances(avd)?;
            let mine = running
                .iter()
                .find(|r| serial.as_deref() == Some(r.serial().as_str()))
                .or_else(|| running.first().filter(|_| *shared));
            Ok(mine.map(|r| Running {
                info: openable(r.info.clone()),
                created: None,
            }))
        }
    }
}

/// Boots the device (creating the simulator first for a new [`Target::IosOwn`]). Android instances
/// are always read-only: the emulator only runs several instances of one AVD when each is.
pub fn boot(target: &Target, progress: &dyn Fn(String)) -> anyhow::Result<Running> {
    match target {
        Target::Ios { udid } => {
            progress("booting the simulator".to_owned());
            boot_ios(udid)?;
            Ok(Running {
                info: ios_info(udid)?,
                created: None,
            })
        }
        Target::IosOwn { udid, base, name } => {
            if let Some(udid) = udid
                && ios_find(udid)?.is_some()
            {
                progress("booting the simulator".to_owned());
                boot_ios(udid)?;
                return Ok(Running {
                    info: ios_info(udid)?,
                    created: None,
                });
            }
            progress("creating the simulator".to_owned());
            let udid = create_ios(base, name)?;
            progress("booting the simulator".to_owned());
            if let Err(err) = boot_ios(&udid) {
                // A clone that never booted is useless: do not leave it behind.
                if let Err(e) = delete_ios(&udid) {
                    tracing::warn!(target: "device_pool", %udid, "could not delete the failed clone: {e:#}");
                }
                return Err(err);
            }
            Ok(Running {
                info: ios_info(&udid)?,
                created: Some(udid),
            })
        }
        Target::Android { avd, .. } => {
            use hangar_devices::android::{AndroidSdk, BootOptions, discovery, emulator};
            let sdk = AndroidSdk::locate()?;
            if let Some(d) = emulator::conflicting_instance(&discovery::scan(), avd, true) {
                return Err(Conflict {
                    avd: avd.clone(),
                    serial: d.serial(),
                }
                .into());
            }
            let options = BootOptions {
                read_only: true,
                ..BootOptions::default()
            };
            let running =
                emulator::boot(&sdk, avd, &options, &|stage| progress(stage.to_string()))?;
            Ok(Running {
                info: openable(running.info),
                created: None,
            })
        }
    }
}

/// Opens a running device on its own input thread.
pub fn open(
    platform: Platform,
    info: DeviceInfo,
    forced: Option<&str>,
) -> anyhow::Result<(DeviceSession, Option<BackendReason>)> {
    let opened = platform.open(info, forced)?;
    Ok((
        DeviceSession::start(Arc::from(opened.device))?,
        opened.reason,
    ))
}

/// Shuts a device down that is not open in the pool.
pub fn shutdown(platform: Platform, id: &str) -> anyhow::Result<()> {
    match platform {
        Platform::Ios => shutdown_ios(id),
        Platform::Android => {
            let sdk = hangar_devices::android::AndroidSdk::locate()?;
            hangar_devices::android::adb::Adb::new(sdk.adb)
                .emu(id, &["kill"])
                .with_context(|| format!("adb -s {id} emu kill"))
        }
    }
}

/// What runs right now: booted simulators, and the read-only emulator instances (the only
/// kind the pool boots) with their AVD.
#[derive(Debug, Default)]
pub struct RunningSet {
    simulators: Vec<String>,
    read_only: Vec<(String, String)>,
}

impl RunningSet {
    pub fn scan() -> Self {
        let simulators = match list_ios_all() {
            Ok(all) => all
                .into_iter()
                .filter(|d| d.state == "Booted")
                .map(|d| d.udid)
                .collect(),
            Err(err) => {
                tracing::debug!(target: "device_pool", "simulators not listed: {err:#}");
                Vec::new()
            }
        };
        let read_only = hangar_devices::android::discovery::scan()
            .into_iter()
            .filter(|d| d.read_only)
            .map(|d| (d.serial(), d.avd_id))
            .collect();
        Self {
            simulators,
            read_only,
        }
    }

    /// The id of the target's running device, if it runs (a pool instance for Android).
    pub fn find(&self, target: &Target) -> Option<String> {
        match target {
            Target::Ios { udid }
            | Target::IosOwn {
                udid: Some(udid), ..
            } => self.simulators.contains(udid).then(|| udid.clone()),
            Target::Android {
                avd,
                serial: Some(serial),
                ..
            } => self
                .read_only
                .iter()
                .any(|(s, a)| s == serial && a == avd)
                .then(|| serial.clone()),
            _ => None,
        }
    }
}

/// Shuts the target down if it runs (a device the pool did not open in this run).
pub fn shutdown_if_running(platform: Platform, target: &Target) -> anyhow::Result<bool> {
    let Some(id) = RunningSet::scan().find(target) else {
        return Ok(false);
    };
    shutdown(platform, &id)?;
    Ok(true)
}

/// Deletes a simulator the pool created (shutting it down first).
pub fn delete_clone(udid: &str) -> anyhow::Result<()> {
    if let Err(err) = shutdown_ios(udid) {
        tracing::warn!(target: "device_pool", %udid, "shutdown before delete: {err:#}");
    }
    delete_ios(udid)
}

/// What a device of this base costs the host.
pub fn cost(platform: Platform, base: Option<&str>) -> DeviceCost {
    match (platform, base, hangar_devices::android::avd::avd_home()) {
        (Platform::Android, Some(avd), Some(home)) => {
            DeviceCost::android(&hangar_devices::android::avd::cost(&home, avd))
        }
        (Platform::Android, _, _) => {
            DeviceCost::android(&hangar_devices::android::AvdCost::DEFAULT)
        }
        (Platform::Ios, _, _) => DeviceCost::ios_simulator(),
    }
}

/// Base devices of `platform` a workspace can be made from, running ones first: simulator
/// UDIDs (not the pool's own clones) or AVD ids (not physical devices).
pub fn bases(platform: Platform) -> anyhow::Result<Vec<(String, String)>> {
    let devices = platform.list()?;
    Ok(base_choices(platform, devices))
}

fn base_choices(platform: Platform, devices: Vec<DeviceInfo>) -> Vec<(String, String)> {
    let mut out: Vec<(bool, String, String)> = Vec::new();
    for d in devices {
        if d.name.starts_with(CLONE_PREFIX) {
            continue;
        }
        let running = platform.is_ready(&d);
        let id = match platform {
            Platform::Ios => d.udid,
            Platform::Android if d.udid.starts_with("emulator-") => match d.device_type {
                Some(avd) => avd,
                None => continue,
            },
            Platform::Android if d.state == hangar_devices::android::state::SHUTDOWN => d.udid,
            Platform::Android => continue,
        };
        if !out.iter().any(|(_, known, _)| *known == id) {
            out.push((running, id, format!("{} · {}", d.name, d.runtime)));
        }
    }
    out.sort_by_key(|(running, _, _)| !*running);
    out.into_iter().map(|(_, id, title)| (id, title)).collect()
}

/// Platforms the apps in `root` build for, in detection order.
pub fn stack_platforms(root: &std::path::Path) -> Vec<Platform> {
    let stacks = hangar_runner::detect(root);
    Platform::ALL
        .into_iter()
        .filter(|p| {
            let p = match p {
                Platform::Ios => hangar_devices::Platform::Ios,
                Platform::Android => hangar_devices::Platform::Android,
            };
            stacks.iter().any(|s| s.targets(p))
        })
        .collect()
}

/// Simulators named [`CLONE_PREFIX`]…, with their state.
pub fn list_clones() -> anyhow::Result<Vec<DeviceInfo>> {
    let mut all = list_ios_all()?;
    all.retain(|d| d.name.starts_with(CLONE_PREFIX));
    Ok(all)
}

/// Android's `resolve` would also match the first running instance of the same AVD through
/// `device_type`; the serial alone identifies the instance.
fn openable(info: DeviceInfo) -> DeviceInfo {
    DeviceInfo {
        device_type: None,
        ..info
    }
}

fn android_instances(avd: &str) -> anyhow::Result<Vec<hangar_devices::android::RunningEmulator>> {
    let sdk = hangar_devices::android::AndroidSdk::locate()?;
    let mut running = hangar_devices::android::list_running(&sdk)?;
    running.retain(|r| r.discovery.avd_id == avd && r.info.state == "Booted");
    Ok(running)
}

#[cfg(target_os = "macos")]
fn list_ios_all() -> anyhow::Result<Vec<DeviceInfo>> {
    Ok(hangar_devices::simctl::list_all()?)
}

fn ios_find(udid: &str) -> anyhow::Result<Option<DeviceInfo>> {
    Ok(list_ios_all()?.into_iter().find(|d| d.udid == udid))
}

fn ios_info(udid: &str) -> anyhow::Result<DeviceInfo> {
    ios_find(udid)?.ok_or_else(|| anyhow!("simulator {udid} not found (deleted?)"))
}

#[cfg(target_os = "macos")]
fn boot_ios(udid: &str) -> anyhow::Result<()> {
    Ok(hangar_devices::ios::boot(udid)?)
}

#[cfg(target_os = "macos")]
fn shutdown_ios(udid: &str) -> anyhow::Result<()> {
    Ok(hangar_devices::ios::shutdown(udid)?)
}

#[cfg(target_os = "macos")]
fn create_ios(base: &str, name: &str) -> anyhow::Result<String> {
    let spec = hangar_devices::simctl::spec(base)?;
    let udid = hangar_devices::simctl::create(name, &spec.device_type, &spec.runtime)?;
    tracing::info!(target: "device_pool", %udid, %name, base, "simulator created");
    Ok(udid)
}

#[cfg(target_os = "macos")]
fn delete_ios(udid: &str) -> anyhow::Result<()> {
    hangar_devices::simctl::delete(udid)?;
    tracing::info!(target: "device_pool", %udid, "simulator deleted");
    Ok(())
}

#[cfg(not(target_os = "macos"))]
fn unsupported<T>() -> anyhow::Result<T> {
    Err(hangar_devices::Error::Unsupported("iOS simulators need macOS").into())
}

#[cfg(not(target_os = "macos"))]
fn list_ios_all() -> anyhow::Result<Vec<DeviceInfo>> {
    unsupported()
}

#[cfg(not(target_os = "macos"))]
fn boot_ios(_: &str) -> anyhow::Result<()> {
    unsupported()
}

#[cfg(not(target_os = "macos"))]
fn shutdown_ios(_: &str) -> anyhow::Result<()> {
    unsupported()
}

#[cfg(not(target_os = "macos"))]
fn create_ios(_: &str, _: &str) -> anyhow::Result<String> {
    unsupported()
}

#[cfg(not(target_os = "macos"))]
fn delete_ios(_: &str) -> anyhow::Result<()> {
    unsupported()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clone_names_are_prefixed() {
        assert_eq!(clone_name("app", "fix-login"), "Hangar · app/fix-login");
        assert!(clone_name("a", "b").starts_with(CLONE_PREFIX));
    }

    fn device(udid: &str, name: &str, state: &str, avd: Option<&str>) -> DeviceInfo {
        DeviceInfo {
            udid: udid.into(),
            name: name.into(),
            runtime: "R".into(),
            state: state.into(),
            device_type: avd.map(Into::into),
            screen: None,
        }
    }

    #[test]
    fn base_choices_skip_clones_and_physical_devices() {
        let ios = base_choices(
            Platform::Ios,
            vec![
                device("A", "iPhone 17", "Shutdown", None),
                device("B", "Hangar · app/x", "Booted", None),
                device("C", "iPhone Air", "Booted", None),
            ],
        );
        let ids: Vec<&str> = ios.iter().map(|(id, _)| id.as_str()).collect();
        assert_eq!(ids, ["C", "A"]);
        let android = base_choices(
            Platform::Android,
            vec![
                device("Small", "Small", "Shutdown", Some("Small")),
                device("emulator-5554", "Pixel", "Booted", Some("Pixel")),
                device("emulator-5556", "Pixel", "Booted", Some("Pixel")),
                device("R5CT", "Galaxy", "Booted", Some("dm3q")),
            ],
        );
        let ids: Vec<&str> = android.iter().map(|(id, _)| id.as_str()).collect();
        assert_eq!(ids, ["Pixel", "Small"]);
    }

    #[test]
    fn openable_drops_the_avd_id() {
        let info = DeviceInfo {
            udid: "emulator-5556".into(),
            name: "Pixel".into(),
            runtime: "Android 17".into(),
            state: "Booted".into(),
            device_type: Some("Pixel_10_Pro".into()),
            screen: None,
        };
        let open = openable(info);
        assert_eq!(open.device_type, None);
        assert_eq!(open.udid, "emulator-5556");
    }
}
