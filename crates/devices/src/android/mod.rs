//! Android emulator backends.
//!
//! Units: [`crate::Input`] coordinates are density-independent pixels (dp), the Android analogue of
//! iOS points; [`crate::ScreenSpec::scale`] is `dpi / 160`, so `width_pt()` is the width in dp and
//! `px = dp * scale`, exactly as for iOS. Frames are in device pixels.

pub mod adb;
pub mod adb_backend;
pub mod avd;
pub mod discovery;
pub mod emulator;
pub mod grpc;
pub mod grpc_backend;
pub mod sdk;

pub use adb_backend::{AdbDevice, Capture};
pub use avd::Avd;
pub use emulator::{BootOptions, RunningEmulator, WindowMode, boot, find_running, list_running};
pub use grpc_backend::{FrameSize, GrpcEmulator};
pub use sdk::AndroidSdk;

use crate::{Device, Result};

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

pub fn open(
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
