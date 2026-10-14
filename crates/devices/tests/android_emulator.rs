//! Needs the Android SDK and a booted emulator started with gRPC, e.g.
//! `cargo run -p hangar-examples --example spike_android -- --boot <AVD> --bench 1`, then
//! `cargo test -p hangar-devices --test android_emulator -- --ignored`.

use std::time::{Duration, Instant};

use hangar_devices::android::{self, state};
use hangar_devices::{Device, DeviceInfo, Frame, Input};

type TestResult<T = ()> = Result<T, Box<dyn std::error::Error>>;

fn booted_emulator() -> TestResult<DeviceInfo> {
    android::list()?
        .into_iter()
        .find(|d| d.udid.starts_with("emulator-") && d.state == state::BOOTED)
        .ok_or_else(|| "boot an emulator first (spike_android --boot <AVD>)".into())
}

fn wait_frame(device: &dyn Device) -> TestResult<Frame> {
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

fn check_frame(info: &DeviceInfo, f: &Frame) -> TestResult {
    let screen = info.screen.ok_or("no screen spec")?;
    assert_eq!((f.width, f.height), (screen.width_px, screen.height_px));
    assert_eq!(f.bgra.len(), f.stride * f.height as usize);
    Ok(())
}

#[test]
#[ignore = "needs the Android SDK and a booted emulator"]
fn lists_running_emulator_with_screen() -> TestResult {
    let d = booted_emulator()?;
    let s = d.screen.ok_or("no screen spec")?;
    assert!(s.scale > 0.0 && s.width_px > 0);
    assert!(d.runtime.starts_with("Android"));
    Ok(())
}

#[test]
#[ignore = "needs the Android SDK and a booted emulator with gRPC"]
fn grpc_backend_frames_and_input() -> TestResult {
    let info = booted_emulator()?;
    let device = android::open_grpc(&info)?;
    assert_eq!(device.backend(), "grpc");
    check_frame(&info, &wait_frame(device.as_ref())?)?;
    device.send_input(Input::Home)?;
    Ok(())
}

#[test]
#[ignore = "needs the Android SDK and a booted emulator"]
fn adb_backend_frames() -> TestResult {
    let info = booted_emulator()?;
    let device = android::open_adb(&info)?;
    assert_eq!(device.backend(), "adb");
    check_frame(&info, &wait_frame(device.as_ref())?)?;
    Ok(())
}

#[test]
#[ignore = "needs the Android SDK and a booted emulator"]
fn launch_settings_via_adb() -> TestResult {
    let device = android::open(&booted_emulator()?)?;
    device.launch("com.android.settings")?;
    assert!(device.launch("com.example.does.not.exist").is_err());
    device.send_input(Input::Home)?;
    Ok(())
}

#[test]
#[ignore = "needs the Android SDK with at least one AVD that is not running"]
fn shut_down_avd_cannot_be_opened() -> TestResult {
    let Some(avd) = android::list()?
        .into_iter()
        .find(|d| d.state == state::SHUTDOWN)
    else {
        return Err("every AVD is running".into());
    };
    assert!(matches!(
        android::open(&avd),
        Err(hangar_devices::Error::NotFound(_))
    ));
    Ok(())
}
