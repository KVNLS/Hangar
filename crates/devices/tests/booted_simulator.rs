//! Needs a booted iOS simulator: `cargo test -p hangar-devices -- --ignored`.
#![cfg(target_os = "macos")]

use std::time::{Duration, Instant};

use hangar_devices::ios::{self, Backend};
use hangar_devices::{DeviceInfo, Frame, simctl};

type TestResult<T = ()> = Result<T, Box<dyn std::error::Error>>;

fn first_booted() -> TestResult<DeviceInfo> {
    simctl::list_booted()?
        .into_iter()
        .next()
        .ok_or_else(|| "boot a simulator first: xcrun simctl boot <UDID>".into())
}

fn wait_frame(backend: Backend) -> TestResult<Frame> {
    let device = ios::open(first_booted()?, backend)?;
    let rx = device.frames();
    let start = Instant::now();
    while start.elapsed() < Duration::from_secs(10) {
        if let Some(f) = rx.borrow().clone() {
            return Ok(f);
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    Err("no frame within 10 s".into())
}

#[test]
#[ignore = "needs a booted iOS simulator"]
fn lists_booted_with_screen_spec() -> TestResult {
    let d = first_booted()?;
    assert!(d.screen.is_some(), "no screen spec for {}", d.name);
    Ok(())
}

#[test]
#[ignore = "needs a booted iOS simulator and Xcode's private frameworks"]
fn private_backend_delivers_frames() -> TestResult {
    let f = wait_frame(Backend::Private)?;
    assert!(f.width > 0 && f.height > 0);
    assert_eq!(f.bgra.len(), f.stride * f.height as usize);
    Ok(())
}

#[test]
#[ignore = "needs a booted iOS simulator"]
fn simctl_backend_delivers_frames() -> TestResult {
    let f = wait_frame(Backend::Simctl)?;
    assert!(f.width > 0 && f.height > 0);
    Ok(())
}
