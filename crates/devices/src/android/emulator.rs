//! Running emulators: listing, boot and shutdown.

use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use super::adb::{Adb, AdbDevice};
use super::avd;
use super::discovery::{self, Discovery};
use super::sdk::AndroidSdk;
use crate::{DeviceInfo, Error, Result, ScreenSpec};

/// An emulator process that advertises itself, joined with its adb state.
#[derive(Debug, Clone, PartialEq)]
pub struct RunningEmulator {
    pub discovery: Discovery,
    pub adb: Option<AdbDevice>,
    pub info: DeviceInfo,
}

impl RunningEmulator {
    pub fn serial(&self) -> String {
        self.discovery.serial()
    }

    pub fn screen(&self) -> Option<ScreenSpec> {
        self.info.screen
    }
}

/// All running emulators (discovery files of live processes), with screen and Android version when
/// adb can reach them.
pub fn list_running(sdk: &AndroidSdk) -> Result<Vec<RunningEmulator>> {
    let adb = Adb::new(sdk.adb.clone());
    // adb being unavailable must not hide emulators that gRPC can still reach.
    let adb_devices = adb.devices().unwrap_or_else(|e| {
        tracing::warn!("adb devices: {e}");
        Vec::new()
    });
    Ok(discovery::scan()
        .into_iter()
        .map(|d| describe(&adb, d, &adb_devices))
        .collect())
}

fn describe(adb: &Adb, d: Discovery, adb_devices: &[AdbDevice]) -> RunningEmulator {
    let serial = d.serial();
    let adb_dev = adb_devices.iter().find(|a| a.serial == serial).cloned();
    let online = adb_dev.as_ref().is_some_and(AdbDevice::is_online);
    let config = d
        .avd_dir
        .as_deref()
        .map(|dir| avd::load_dir(&d.avd_id, dir));
    let screen = online
        .then(|| adb.screen(&serial).ok())
        .flatten()
        .and_then(|(w, h, dpi)| avd::screen_from_density(w, h, dpi))
        .or_else(|| config.as_ref().and_then(|c| c.screen));
    let runtime = match online.then(|| adb.version(&serial).ok()).flatten() {
        Some((release, sdk)) if !release.is_empty() => format!("Android {release} (API {sdk})"),
        _ => config
            .as_ref()
            .and_then(|c| c.target.as_deref())
            .map_or_else(|| "Android".to_owned(), avd::target_label),
    };
    let state = device_state(adb, adb_dev.as_ref()).to_owned();
    let info = DeviceInfo {
        udid: serial,
        name: d.avd_name.clone(),
        runtime,
        state,
        device_type: Some(d.avd_id.clone()),
        screen,
    };
    RunningEmulator {
        discovery: d,
        adb: adb_dev,
        info,
    }
}

/// `DeviceInfo::state` values used for Android.
pub mod state {
    /// Online in adb and `sys.boot_completed=1`.
    pub const BOOTED: &str = "Booted";
    /// Online in adb, Android still starting.
    pub const BOOTING: &str = "Booting";
    /// Process running (or device attached) but adb cannot talk to it yet or any more.
    pub const OFFLINE: &str = "Offline";
    /// AVD not running.
    pub const SHUTDOWN: &str = "Shutdown";
}

pub(crate) fn device_state(adb: &Adb, dev: Option<&AdbDevice>) -> &'static str {
    match dev {
        Some(a) if a.is_online() && adb.boot_completed(&a.serial) => state::BOOTED,
        Some(a) if a.is_online() => state::BOOTING,
        _ => state::OFFLINE,
    }
}

/// A non-emulator adb device (USB or Wi-Fi phone), for the adb backend.
pub fn describe_physical(adb: &Adb, dev: &AdbDevice) -> DeviceInfo {
    let online = dev.is_online();
    let screen = online
        .then(|| adb.screen(&dev.serial).ok())
        .flatten()
        .and_then(|(w, h, dpi)| avd::screen_from_density(w, h, dpi));
    let runtime = match online.then(|| adb.version(&dev.serial).ok()).flatten() {
        Some((release, sdk)) if !release.is_empty() => format!("Android {release} (API {sdk})"),
        _ => "Android".to_owned(),
    };
    DeviceInfo {
        udid: dev.serial.clone(),
        name: dev
            .model
            .as_deref()
            .map_or_else(|| dev.serial.clone(), |m| m.replace('_', " ")),
        runtime,
        state: device_state(adb, Some(dev)).to_owned(),
        device_type: dev.device.clone(),
        screen,
    }
}

/// Finds the running emulator for an adb serial (`emulator-5554`) or an AVD id.
pub fn find_running(sdk: &AndroidSdk, serial_or_avd: &str) -> Result<RunningEmulator> {
    list_running(sdk)?
        .into_iter()
        .find(|e| e.serial() == serial_or_avd || e.discovery.avd_id == serial_or_avd)
        .ok_or_else(|| Error::NotFound(format!("running emulator {serial_or_avd}")))
}

/// How the emulator renders without a window.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WindowMode {
    /// `-no-window`: the headless binary. Software GPU (SwiftShader/lavapipe) on macOS, so snapshots
    /// taken by Android Studio's windowed emulator do not load ("different renderer").
    NoWindow,
    /// `-qt-hide-window`: the regular binary with its window hidden; keeps the host GPU and the
    /// snapshots Android Studio made.
    HiddenWindow,
}

#[derive(Debug, Clone)]
pub struct BootOptions {
    pub window: WindowMode,
    /// Skip loading the quick-boot snapshot (`-no-snapshot-load`).
    pub cold_boot: bool,
    /// Save a quick-boot snapshot on graceful shutdown (default off: `-no-snapshot-save`, so shutdown
    /// is fast and the AVD state Android Studio relies on is not overwritten).
    pub save_snapshot: bool,
    /// Fixed gRPC port; by default the first free port from 8554 upwards.
    pub grpc_port: Option<u16>,
    /// Until `sys.boot_completed=1`.
    pub timeout: Duration,
    /// Where the emulator's stdout/stderr go; default `$TMPDIR/hangar-emulator-<avd>.log`.
    pub log_file: Option<PathBuf>,
    pub extra_args: Vec<String>,
}

impl Default for BootOptions {
    fn default() -> Self {
        Self {
            window: WindowMode::HiddenWindow,
            cold_boot: false,
            save_snapshot: false,
            grpc_port: None,
            timeout: Duration::from_secs(240),
            log_file: None,
            extra_args: Vec::new(),
        }
    }
}

/// Emulator command line for `avd_id` (pure, for tests).
pub fn boot_args(avd_id: &str, grpc_port: u16, opts: &BootOptions) -> Vec<String> {
    let mut args: Vec<String> = vec![
        "-avd".into(),
        avd_id.into(),
        "-grpc".into(),
        grpc_port.to_string(),
        // Token auth: the token is in the discovery file, readable only by this user.
        "-grpc-use-token".into(),
        "-no-boot-anim".into(),
    ];
    args.push(
        match opts.window {
            WindowMode::NoWindow => "-no-window",
            WindowMode::HiddenWindow => "-qt-hide-window",
        }
        .into(),
    );
    if opts.cold_boot {
        args.push("-no-snapshot-load".into());
    }
    if !opts.save_snapshot {
        args.push("-no-snapshot-save".into());
    }
    args.extend(opts.extra_args.iter().cloned());
    args
}

fn free_grpc_port(taken: &[u16]) -> Result<u16> {
    (8554..8754)
        .find(|p| !taken.contains(p) && std::net::TcpListener::bind(("127.0.0.1", *p)).is_ok())
        .ok_or_else(|| Error::Command("no free gRPC port in 8554..8754".into()))
}

/// Launches `avd_id` detached from our process group and waits until Android has booted.
/// Blocking: call it from a background thread.
pub fn boot(sdk: &AndroidSdk, avd_id: &str, opts: &BootOptions) -> Result<RunningEmulator> {
    let running = discovery::scan();
    if let Some(d) = running.iter().find(|d| d.avd_id == avd_id) {
        return Err(Error::Command(format!(
            "{avd_id} is already running as {}",
            d.serial()
        )));
    }
    let taken: Vec<u16> = running.iter().filter_map(|d| d.grpc_port).collect();
    let port = match opts.grpc_port {
        Some(p) => p,
        None => free_grpc_port(&taken)?,
    };
    let log_path = opts
        .log_file
        .clone()
        .unwrap_or_else(|| std::env::temp_dir().join(format!("hangar-emulator-{avd_id}.log")));
    let log = std::fs::File::create(&log_path)?;
    let mut cmd = Command::new(&sdk.emulator);
    cmd.args(boot_args(avd_id, port, opts))
        .stdin(Stdio::null())
        .stdout(log.try_clone()?)
        .stderr(log);
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt as _;
        // Own process group: a Ctrl-C in the terminal running Hangar must not kill the emulator.
        cmd.process_group(0);
    }
    let started = Instant::now();
    let mut child = cmd
        .spawn()
        .map_err(|e| Error::Command(format!("{}: {e}", sdk.emulator.display())))?;
    tracing::info!(avd_id, port, log = %log_path.display(), "emulator launched");

    let log_tail = || {
        let text = std::fs::read_to_string(&log_path).unwrap_or_default();
        let lines: Vec<&str> = text.lines().rev().take(8).collect();
        lines.into_iter().rev().collect::<Vec<_>>().join("\n")
    };
    let deadline = started + opts.timeout;
    let found = loop {
        if let Some(d) = discovery::scan()
            .into_iter()
            .find(|d| d.grpc_port == Some(port) && d.avd_id == avd_id)
        {
            break d;
        }
        if let Ok(Some(status)) = child.try_wait()
            && !status.success()
        {
            return Err(Error::Command(format!(
                "emulator exited with {status}:\n{}",
                log_tail()
            )));
        }
        if Instant::now() > deadline {
            return Err(Error::Command(format!(
                "emulator did not advertise itself within {:?}:\n{}",
                opts.timeout,
                log_tail()
            )));
        }
        std::thread::sleep(Duration::from_millis(200));
    };
    // Reap the launcher whenever it exits so it never lingers as a zombie.
    let _ = std::thread::Builder::new()
        .name("emulator-reaper".into())
        .spawn(move || {
            let _ = child.wait();
        });

    let adb = Adb::new(sdk.adb.clone());
    let serial = found.serial();
    loop {
        if adb.boot_completed(&serial) {
            break;
        }
        if !discovery::process_alive(found.pid) {
            return Err(Error::Command(format!(
                "emulator stopped while booting:\n{}",
                log_tail()
            )));
        }
        if Instant::now() > deadline {
            return Err(Error::Command(format!(
                "{serial} not booted within {:?}",
                opts.timeout
            )));
        }
        std::thread::sleep(Duration::from_millis(250));
    }
    tracing::info!(serial, elapsed = ?started.elapsed(), "emulator booted");
    let adb_devices = adb.devices().unwrap_or_default();
    Ok(describe(&adb, found, &adb_devices))
}

/// Waits for an emulator process to exit (discovery file pid gone).
pub fn wait_exit(pid: u32, timeout: Duration) -> bool {
    let start = Instant::now();
    while start.elapsed() < timeout {
        if !discovery::process_alive(pid) {
            return true;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    false
}

/// Shuts an emulator down through its console (`adb emu kill`) and waits for the process to exit.
pub fn kill_via_adb(sdk: &AndroidSdk, d: &Discovery, timeout: Duration) -> Result<()> {
    Adb::new(sdk.adb.clone()).emu(&d.serial(), &["kill"])?;
    if wait_exit(d.pid, timeout) {
        Ok(())
    } else {
        Err(Error::Command(format!(
            "{} still running {timeout:?} after `adb emu kill`",
            d.serial()
        )))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn boot_args_default_and_cold() {
        let a = boot_args("Pixel_10_Pro", 8554, &BootOptions::default());
        assert_eq!(
            a,
            [
                "-avd",
                "Pixel_10_Pro",
                "-grpc",
                "8554",
                "-grpc-use-token",
                "-no-boot-anim",
                "-qt-hide-window",
                "-no-snapshot-save"
            ]
        );
        let cold = BootOptions {
            window: WindowMode::NoWindow,
            cold_boot: true,
            save_snapshot: true,
            ..BootOptions::default()
        };
        let a = boot_args("X", 8600, &cold);
        assert!(a.contains(&"-no-window".to_owned()));
        assert!(a.contains(&"-no-snapshot-load".to_owned()));
        assert!(!a.contains(&"-no-snapshot-save".to_owned()));
    }

    #[test]
    fn free_port_skips_taken() -> Result<()> {
        let p = free_grpc_port(&[8554, 8555])?;
        assert!(p > 8555);
        Ok(())
    }
}
