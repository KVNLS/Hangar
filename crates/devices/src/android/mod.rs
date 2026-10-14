//! Android emulator (and adb device) backends.
//!
//! Units: [`crate::Input`] coordinates are density-independent pixels (dp), the Android analogue of
//! iOS points; [`crate::ScreenSpec::scale`] is `dpi / 160`, so `width_pt()` is the width in dp and
//! `px = dp * scale`, exactly as for iOS. Frames are in device pixels.
//!
//! High-level, blocking entry points (call them off the UI thread): [`list`], [`open`],
//! [`open_grpc`], [`open_adb`], [`boot`].

pub mod adb;
pub mod adb_backend;
pub mod avd;
pub mod discovery;
pub mod emulator;
pub mod grpc;
pub mod grpc_backend;
pub mod logcat;
pub mod sdk;

pub use adb_backend::{AdbDevice, Capture};
pub use avd::Avd;
pub use emulator::{BootOptions, RunningEmulator, WindowMode, find_running, list_running, state};
pub use grpc_backend::{FrameSize, GrpcEmulator};
pub use logcat::logcat;
pub use sdk::AndroidSdk;

use crate::{Device, DeviceInfo, Error, Result};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Backend {
    /// Emulator gRPC: `streamScreenshot` frames, `sendTouch`/`sendKey` input.
    Grpc,
    /// adb only: polled `screencap`, `input` commands.
    Adb,
}

impl std::str::FromStr for Backend {
    type Err = String;

    fn from_str(s: &str) -> std::result::Result<Self, Self::Err> {
        match s {
            "grpc" => Ok(Self::Grpc),
            "adb" => Ok(Self::Adb),
            other => Err(format!("unknown backend {other:?} (expected grpc|adb)")),
        }
    }
}

/// All AVDs (`emulator -list-avds`).
pub fn list_avds(sdk: &AndroidSdk) -> Result<Vec<Avd>> {
    avd::list(sdk)
}

/// Running emulators (`udid` = adb serial, e.g. `emulator-5554`), attached physical devices
/// (`udid` = adb serial) and AVDs that are not running (`udid` = AVD id, state
/// [`state::SHUTDOWN`]). `device_type` is the AVD id for emulators.
pub fn list() -> Result<Vec<DeviceInfo>> {
    let sdk = AndroidSdk::locate()?;
    let running = list_running(&sdk)?;
    let adb = adb::Adb::new(sdk.adb.clone());
    let mut out: Vec<DeviceInfo> = running.iter().map(|r| r.info.clone()).collect();
    for dev in adb.devices().unwrap_or_default() {
        if !dev.is_emulator() {
            out.push(emulator::describe_physical(&adb, &dev));
        }
    }
    for a in list_avds(&sdk)? {
        if running.iter().any(|r| r.discovery.avd_id == a.id) {
            continue;
        }
        out.push(DeviceInfo {
            udid: a.id.clone(),
            name: a.name,
            runtime: a
                .target
                .as_deref()
                .map_or_else(|| "Android".to_owned(), avd::target_label),
            state: state::SHUTDOWN.to_owned(),
            device_type: Some(a.id),
            screen: a.screen,
        });
    }
    Ok(out)
}

/// Opens a device from [`list`]: gRPC for emulators, falling back to adb if gRPC fails;
/// adb for physical devices. AVDs that are not running return [`Error::NotFound`] (boot first).
pub fn open(info: &DeviceInfo) -> Result<Box<dyn Device>> {
    let sdk = AndroidSdk::locate()?;
    match resolve(&sdk, info)? {
        Target::Emulator(running) => match GrpcEmulator::open(&sdk, (*running).clone()) {
            Ok(d) => Ok(Box::new(d)),
            Err(e) => {
                tracing::warn!("gRPC unavailable for {} ({e}), using adb", running.serial());
                open_with(&sdk, *running, Backend::Adb)
            }
        },
        Target::Physical(info) => Ok(Box::new(AdbDevice::open(
            adb::Adb::new(sdk.adb.clone()),
            info,
            Capture::default(),
        )?)),
    }
}

/// Opens an emulator with the gRPC backend only.
pub fn open_grpc(info: &DeviceInfo) -> Result<Box<dyn Device>> {
    let sdk = AndroidSdk::locate()?;
    match resolve(&sdk, info)? {
        Target::Emulator(running) => open_with(&sdk, *running, Backend::Grpc),
        Target::Physical(_) => Err(Error::Unsupported("gRPC backend on a physical device")),
    }
}

/// Opens an emulator or physical device with the adb backend only.
pub fn open_adb(info: &DeviceInfo) -> Result<Box<dyn Device>> {
    let sdk = AndroidSdk::locate()?;
    let info = match resolve(&sdk, info)? {
        Target::Emulator(running) => running.info,
        Target::Physical(info) => info,
    };
    Ok(Box::new(AdbDevice::open(
        adb::Adb::new(sdk.adb.clone()),
        info,
        Capture::default(),
    )?))
}

/// Boots an AVD with [`BootOptions::default`] and returns it as listed by [`list`] once Android
/// has booted. Blocks for the whole boot (seconds to minutes).
pub fn boot(avd_id: &str) -> Result<DeviceInfo> {
    let sdk = AndroidSdk::locate()?;
    Ok(boot_with(&sdk, avd_id, &BootOptions::default())?.info)
}

pub fn boot_with(sdk: &AndroidSdk, avd_id: &str, options: &BootOptions) -> Result<RunningEmulator> {
    emulator::boot(sdk, avd_id, options)
}

pub fn open_with(
    sdk: &AndroidSdk,
    running: RunningEmulator,
    backend: Backend,
) -> Result<Box<dyn Device>> {
    Ok(match backend {
        Backend::Grpc => Box::new(GrpcEmulator::open(sdk, running)?),
        Backend::Adb => Box::new(AdbDevice::open(
            adb::Adb::new(sdk.adb.clone()),
            running.info,
            Capture::default(),
        )?),
    })
}

enum Target {
    Emulator(Box<RunningEmulator>),
    Physical(DeviceInfo),
}

fn resolve(sdk: &AndroidSdk, info: &DeviceInfo) -> Result<Target> {
    let running = list_running(sdk)?;
    if let Some(r) = running.into_iter().find(|r| {
        r.serial() == info.udid
            || r.discovery.avd_id == info.udid
            || (info.state != state::SHUTDOWN
                && info.device_type.as_deref() == Some(r.discovery.avd_id.as_str()))
    }) {
        return Ok(Target::Emulator(Box::new(r)));
    }
    let adb = adb::Adb::new(sdk.adb.clone());
    if let Some(dev) = adb
        .devices()?
        .into_iter()
        .find(|d| d.serial == info.udid && !d.is_emulator())
    {
        return Ok(Target::Physical(emulator::describe_physical(&adb, &dev)));
    }
    Err(Error::NotFound(format!(
        "{} ({}) is not running; boot it first",
        info.name, info.udid
    )))
}
