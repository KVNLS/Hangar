//! The devices crate's types and platform helpers that the app names directly. Types that
//! `hangar_core::device` also exports are used from there.

pub use hangar_devices::{
    Device, Error, FrameReceiver, LogEntry, LogFilter, LogStream, Result, SavePng, android,
    geometry, simctl,
};

#[cfg(target_os = "macos")]
pub use hangar_devices::ios;
