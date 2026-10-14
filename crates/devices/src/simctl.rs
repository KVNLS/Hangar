//! `xcrun simctl` JSON parsing (pure, testable anywhere) and invocation (macOS only).

use std::collections::BTreeMap;

use serde::Deserialize;

use crate::{DeviceInfo, Error, Result, ScreenSpec};

pub use hangar_core::device::state;

#[derive(Deserialize)]
struct DeviceList {
    devices: BTreeMap<String, Vec<SimctlDevice>>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct SimctlDevice {
    udid: String,
    name: String,
    state: String,
    #[serde(default)]
    device_type_identifier: Option<String>,
    #[serde(default)]
    is_available: Option<bool>,
}

/// Parses `xcrun simctl list devices [booted] -j`; returns only booted devices.
pub fn parse_booted(json: &str) -> Result<Vec<DeviceInfo>> {
    let list: DeviceList = serde_json::from_str(json).map_err(|e| Error::Parse(e.to_string()))?;
    let mut out = Vec::new();
    for (runtime, devices) in list.devices {
        for d in devices.into_iter().filter(|d| d.state == state::BOOTED) {
            out.push(DeviceInfo {
                udid: d.udid,
                name: d.name,
                runtime: runtime_name(&runtime),
                state: d.state,
                device_type: d.device_type_identifier,
                screen: None,
            });
        }
    }
    Ok(out)
}

/// Parses `xcrun simctl list devices -j`; returns every available device in any state
/// (devices of unavailable runtimes are dropped).
pub fn parse_all(json: &str) -> Result<Vec<DeviceInfo>> {
    let list: DeviceList = serde_json::from_str(json).map_err(|e| Error::Parse(e.to_string()))?;
    let mut out = Vec::new();
    for (runtime, devices) in list.devices {
        for d in devices
            .into_iter()
            .filter(|d| d.is_available != Some(false))
        {
            out.push(DeviceInfo {
                udid: d.udid,
                name: d.name,
                runtime: runtime_name(&runtime),
                state: d.state,
                device_type: d.device_type_identifier,
                screen: None,
            });
        }
    }
    Ok(out)
}

/// `com.apple.CoreSimulator.SimRuntime.iOS-27-0` → `iOS 27.0`.
pub fn runtime_name(identifier: &str) -> String {
    let tail = identifier.rsplit('.').next().unwrap_or(identifier);
    match tail.split_once('-') {
        Some((os, version)) => format!("{os} {}", version.replace('-', ".")),
        None => tail.to_owned(),
    }
}

/// What `simctl create` needs to make another simulator like an existing one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SimSpec {
    /// `com.apple.CoreSimulator.SimDeviceType.iPhone-17`.
    pub device_type: String,
    /// `com.apple.CoreSimulator.SimRuntime.iOS-27-0`.
    pub runtime: String,
}

/// Device type and runtime identifiers of `udid` in `xcrun simctl list devices -j`.
pub fn parse_spec(json: &str, udid: &str) -> Result<SimSpec> {
    let list: DeviceList = serde_json::from_str(json).map_err(|e| Error::Parse(e.to_string()))?;
    for (runtime, devices) in list.devices {
        if let Some(d) = devices.into_iter().find(|d| d.udid == udid) {
            let device_type = d
                .device_type_identifier
                .ok_or_else(|| Error::Parse(format!("{udid}: no deviceTypeIdentifier")))?;
            return Ok(SimSpec {
                device_type,
                runtime,
            });
        }
    }
    Err(Error::NotFound(udid.to_owned()))
}

/// `simctl create` prints the new device's UDID on its own line.
pub fn parse_create_udid(stdout: &str) -> Result<String> {
    let line = stdout.trim();
    let is_udid = line.len() == 36
        && line.chars().enumerate().all(|(i, c)| match i {
            8 | 13 | 18 | 23 => c == '-',
            _ => c.is_ascii_hexdigit(),
        });
    if is_udid {
        Ok(line.to_owned())
    } else {
        Err(Error::Parse(format!("simctl create output: {line:?}")))
    }
}

#[derive(Deserialize)]
struct DeviceTypes {
    devicetypes: Vec<DeviceType>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct DeviceType {
    identifier: String,
    bundle_path: String,
}

/// Parses `xcrun simctl list devicetypes -j` into identifier → bundle path.
pub fn parse_device_types(json: &str) -> Result<BTreeMap<String, String>> {
    let types: DeviceTypes = serde_json::from_str(json).map_err(|e| Error::Parse(e.to_string()))?;
    Ok(types
        .devicetypes
        .into_iter()
        .map(|t| (t.identifier, t.bundle_path))
        .collect())
}

#[derive(Deserialize)]
struct Capabilities {
    capabilities: CapabilitiesInner,
}

#[derive(Deserialize)]
#[serde(rename_all = "PascalCase")]
struct CapabilitiesInner {
    screen_dimensions_capability: ScreenDimensions,
}

#[derive(Deserialize)]
#[serde(rename_all = "kebab-case")]
struct ScreenDimensions {
    main_screen_width: u32,
    main_screen_height: u32,
    main_screen_scale: f64,
}

/// Parses a device type's `capabilities.plist` (converted to JSON with `plutil`).
pub fn parse_screen_spec(json: &str) -> Result<ScreenSpec> {
    let caps: Capabilities = serde_json::from_str(json).map_err(|e| Error::Parse(e.to_string()))?;
    let d = caps.capabilities.screen_dimensions_capability;
    if d.main_screen_scale <= 0.0 || d.main_screen_width == 0 || d.main_screen_height == 0 {
        return Err(Error::Parse("invalid screen dimensions".into()));
    }
    Ok(ScreenSpec {
        width_px: d.main_screen_width,
        height_px: d.main_screen_height,
        scale: d.main_screen_scale,
    })
}

#[cfg(target_os = "macos")]
fn run(program: &str, args: &[&str]) -> Result<Vec<u8>> {
    let out = std::process::Command::new(program).args(args).output()?;
    if !out.status.success() {
        return Err(Error::Command(format!(
            "{program} {}: {}",
            args.join(" "),
            String::from_utf8_lossy(&out.stderr).trim()
        )));
    }
    Ok(out.stdout)
}

#[derive(Deserialize)]
struct RuntimeList {
    runtimes: Vec<SimctlRuntime>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct SimctlRuntime {
    #[serde(default)]
    platform: Option<String>,
    #[serde(default)]
    identifier: String,
    #[serde(default)]
    is_available: Option<bool>,
}

/// Parses `xcrun simctl list runtimes -j`; counts the available iOS runtimes.
pub fn count_ios_runtimes(json: &str) -> Result<usize> {
    let list: RuntimeList = serde_json::from_str(json).map_err(|e| Error::Parse(e.to_string()))?;
    Ok(list
        .runtimes
        .iter()
        .filter(|r| r.is_available != Some(false))
        .filter(|r| match &r.platform {
            Some(platform) => platform == "iOS",
            None => r.identifier.contains(".SimRuntime.iOS-"),
        })
        .count())
}

/// How many iOS simulator runtimes are installed.
#[cfg(target_os = "macos")]
pub fn ios_runtime_count() -> Result<usize> {
    let json = run("xcrun", &["simctl", "list", "runtimes", "-j"])?;
    count_ios_runtimes(&String::from_utf8_lossy(&json))
}

/// Lists booted simulators with their screen geometry filled in when available.
#[cfg(target_os = "macos")]
pub fn list_booted() -> Result<Vec<DeviceInfo>> {
    let json = run("xcrun", &["simctl", "list", "devices", "booted", "-j"])?;
    let mut devices = parse_booted(&String::from_utf8_lossy(&json))?;
    fill_screens(&mut devices)?;
    Ok(devices)
}

/// Lists every available simulator (booted or not) with its screen geometry filled in when
/// available. Entries are grouped by runtime identifier, in `simctl` order within a runtime.
#[cfg(target_os = "macos")]
pub fn list_all() -> Result<Vec<DeviceInfo>> {
    let json = run("xcrun", &["simctl", "list", "devices", "-j"])?;
    let mut devices = parse_all(&String::from_utf8_lossy(&json))?;
    fill_screens(&mut devices)?;
    Ok(devices)
}

#[cfg(target_os = "macos")]
fn fill_screens(devices: &mut [DeviceInfo]) -> Result<()> {
    if devices.is_empty() {
        return Ok(());
    }
    let types_json = run("xcrun", &["simctl", "list", "devicetypes", "-j"])?;
    let types = parse_device_types(&String::from_utf8_lossy(&types_json))?;
    // Many simulators share a device type; read each capabilities.plist once.
    let mut specs: BTreeMap<String, Option<ScreenSpec>> = BTreeMap::new();
    for d in devices {
        let Some(ty) = d.device_type.as_ref() else {
            continue;
        };
        let Some(bundle) = types.get(ty) else {
            continue;
        };
        let spec = specs.entry(ty.clone()).or_insert_with(|| {
            let plist = format!("{bundle}/Contents/Resources/capabilities.plist");
            match run("plutil", &["-convert", "json", "-o", "-", &plist])
                .and_then(|j| parse_screen_spec(&String::from_utf8_lossy(&j)))
            {
                Ok(spec) => Some(spec),
                Err(e) => {
                    tracing::warn!("no screen spec for {}: {e}", d.name);
                    None
                }
            }
        });
        d.screen = *spec;
    }
    Ok(())
}

/// Upper bound for `simctl bootstatus -b` (first boot of a fresh runtime can take minutes).
#[cfg(target_os = "macos")]
const BOOT_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(180);

/// Boots the simulator (if needed) and waits until it finished booting (`bootstatus -b`).
/// Returns immediately when it is already booted.
#[cfg(target_os = "macos")]
pub fn boot(udid: &str) -> Result<()> {
    run_timeout("xcrun", &["simctl", "bootstatus", udid, "-b"], BOOT_TIMEOUT)
}

/// `simctl create`: a new simulator named `name`; returns its UDID.
#[cfg(target_os = "macos")]
pub fn create(name: &str, device_type: &str, runtime: &str) -> Result<String> {
    let out = run("xcrun", &["simctl", "create", name, device_type, runtime])?;
    parse_create_udid(&String::from_utf8_lossy(&out))
}

/// `simctl delete`: removes the simulator and its data (it must not be booted).
#[cfg(target_os = "macos")]
pub fn delete(udid: &str) -> Result<()> {
    run("xcrun", &["simctl", "delete", udid]).map(|_| ())
}

/// Device type and runtime of an existing simulator, to create more like it.
#[cfg(target_os = "macos")]
pub fn spec(udid: &str) -> Result<SimSpec> {
    let json = run("xcrun", &["simctl", "list", "devices", "-j"])?;
    parse_spec(&String::from_utf8_lossy(&json), udid)
}

/// Shuts the simulator down; already-shutdown devices are not an error.
#[cfg(target_os = "macos")]
pub fn shutdown(udid: &str) -> Result<()> {
    match run("xcrun", &["simctl", "shutdown", udid]) {
        Err(Error::Command(m)) if m.contains("current state: Shutdown") => Ok(()),
        other => other.map(|_| ()),
    }
}

/// `simctl install`: installs a `.app` bundle on a booted simulator.
#[cfg(target_os = "macos")]
pub fn install(udid: &str, app_path: &std::path::Path) -> Result<()> {
    let path = app_path
        .to_str()
        .ok_or_else(|| Error::Parse("app path is not valid UTF-8".into()))?;
    run("xcrun", &["simctl", "install", udid, path]).map(|_| ())
}

/// `simctl launch`: starts an installed app; returns its pid when simctl reports one.
#[cfg(target_os = "macos")]
pub fn launch(udid: &str, bundle_id: &str) -> Result<Option<u32>> {
    let out = run("xcrun", &["simctl", "launch", udid, bundle_id])?;
    Ok(parse_launch_pid(&String::from_utf8_lossy(&out)))
}

/// `com.example.app: 1234` → `1234`.
pub fn parse_launch_pid(stdout: &str) -> Option<u32> {
    stdout.trim().rsplit_once(':')?.1.trim().parse().ok()
}

/// Like [`run`] but kills the child after `timeout`.
#[cfg(target_os = "macos")]
fn run_timeout(program: &str, args: &[&str], timeout: std::time::Duration) -> Result<()> {
    use std::process::Stdio;
    let mut child = std::process::Command::new(program)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()?;
    let deadline = std::time::Instant::now() + timeout;
    let status = loop {
        if let Some(status) = child.try_wait()? {
            break status;
        }
        if std::time::Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            return Err(Error::Command(format!(
                "{program} {}: timed out after {}s",
                args.join(" "),
                timeout.as_secs()
            )));
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    };
    if status.success() {
        return Ok(());
    }
    let mut stderr = String::new();
    if let Some(mut e) = child.stderr.take() {
        let _ = std::io::Read::read_to_string(&mut e, &mut stderr);
    }
    Err(Error::Command(format!(
        "{program} {}: {}",
        args.join(" "),
        stderr.trim()
    )))
}

#[cfg(test)]
mod tests {
    use super::*;

    const BOOTED: &str = r#"{
      "devices" : {
        "com.apple.CoreSimulator.SimRuntime.iOS-27-0" : [
          {
            "dataPath" : "\/Users\/x\/data",
            "udid" : "96D76D05-5A8F-483B-815A-DBBF1C32E4DB",
            "isAvailable" : true,
            "deviceTypeIdentifier" : "com.apple.CoreSimulator.SimDeviceType.iPhone-17",
            "state" : "Booted",
            "name" : "iPhone 17"
          },
          {
            "udid" : "209369F7-09AB-40DD-BF6F-94AF43FFAEF9",
            "isAvailable" : true,
            "state" : "Shutdown",
            "name" : "iPhone 18 Pro"
          }
        ],
        "com.apple.CoreSimulator.SimRuntime.iOS-26-5" : []
      }
    }"#;

    #[test]
    fn parses_booted_devices_only() {
        let devices = parse_booted(BOOTED).unwrap();
        assert_eq!(devices.len(), 1);
        let d = &devices[0];
        assert_eq!(d.udid, "96D76D05-5A8F-483B-815A-DBBF1C32E4DB");
        assert_eq!(d.name, "iPhone 17");
        assert_eq!(d.runtime, "iOS 27.0");
        assert_eq!(
            d.device_type.as_deref(),
            Some("com.apple.CoreSimulator.SimDeviceType.iPhone-17")
        );
    }

    #[test]
    fn parses_all_available_devices() {
        let json = r#"{"devices":{
          "com.apple.CoreSimulator.SimRuntime.iOS-27-0":[
            {"udid":"A","isAvailable":true,"deviceTypeIdentifier":"com.apple.CoreSimulator.SimDeviceType.iPhone-17","state":"Booted","name":"iPhone 17"},
            {"udid":"B","isAvailable":true,"state":"Shutdown","name":"iPhone 18 Pro"},
            {"udid":"C","isAvailable":false,"state":"Shutdown","name":"Broken"},
            {"udid":"D","state":"Shutting Down","name":"No availability key"}
          ],
          "com.apple.CoreSimulator.SimRuntime.iOS-26-5":[]}}"#;
        let all = parse_all(json).unwrap();
        let ids: Vec<_> = all
            .iter()
            .map(|d| (d.udid.as_str(), d.state.as_str()))
            .collect();
        assert_eq!(
            ids,
            [("A", "Booted"), ("B", "Shutdown"), ("D", "Shutting Down")]
        );
        assert_eq!(all[1].runtime, "iOS 27.0");
        assert!(all[1].device_type.is_none());
        assert!(matches!(parse_all("{"), Err(Error::Parse(_))));
    }

    #[test]
    fn spec_of_a_device() {
        let spec = parse_spec(BOOTED, "96D76D05-5A8F-483B-815A-DBBF1C32E4DB").unwrap();
        assert_eq!(
            spec,
            SimSpec {
                device_type: "com.apple.CoreSimulator.SimDeviceType.iPhone-17".into(),
                runtime: "com.apple.CoreSimulator.SimRuntime.iOS-27-0".into(),
            }
        );
        assert!(matches!(
            parse_spec(BOOTED, "209369F7-09AB-40DD-BF6F-94AF43FFAEF9"),
            Err(Error::Parse(_))
        ));
        assert!(matches!(
            parse_spec(BOOTED, "missing"),
            Err(Error::NotFound(_))
        ));
    }

    #[test]
    fn create_udid() {
        assert_eq!(
            parse_create_udid("6F1B4E27-3C0A-4F55-9D2E-0A1B2C3D4E5F\n").unwrap(),
            "6F1B4E27-3C0A-4F55-9D2E-0A1B2C3D4E5F"
        );
        assert!(matches!(
            parse_create_udid("Invalid device type: x\n"),
            Err(Error::Parse(_))
        ));
        assert!(parse_create_udid("").is_err());
    }

    #[test]
    fn launch_pid() {
        assert_eq!(
            parse_launch_pid("com.apple.mobilesafari: 4242\n"),
            Some(4242)
        );
        assert_eq!(parse_launch_pid("garbage"), None);
    }

    #[test]
    fn empty_and_invalid_json() {
        assert!(parse_booted(r#"{"devices":{}}"#).unwrap().is_empty());
        assert!(matches!(parse_booted("nope"), Err(Error::Parse(_))));
    }

    #[test]
    fn runtime_names() {
        assert_eq!(
            runtime_name("com.apple.CoreSimulator.SimRuntime.iOS-26-5"),
            "iOS 26.5"
        );
        assert_eq!(
            runtime_name("com.apple.CoreSimulator.SimRuntime.watchOS-11-0"),
            "watchOS 11.0"
        );
    }

    #[test]
    fn parses_device_types() {
        let json = r#"{"devicetypes":[{"productFamily":"iPhone","bundlePath":"\/Library\/X\/iPhone 17.simdevicetype","identifier":"com.apple.CoreSimulator.SimDeviceType.iPhone-17","name":"iPhone 17"}]}"#;
        let t = parse_device_types(json).unwrap();
        assert_eq!(
            t.get("com.apple.CoreSimulator.SimDeviceType.iPhone-17")
                .map(String::as_str),
            Some("/Library/X/iPhone 17.simdevicetype")
        );
    }

    #[test]
    fn parses_screen_spec() {
        let json = r#"{"capabilities":{"ScreenDimensionsCapability":{"main-screen-scale":3,"main-screen-height":2622,"main-screen-pitch":460,"main-screen-width":1206}},"iconState":{}}"#;
        let s = parse_screen_spec(json).unwrap();
        assert_eq!(s.width_px, 1206);
        assert_eq!(s.height_px, 2622);
        assert_eq!(s.scale, 3.0);
        assert_eq!(s.width_pt(), 402.0);
        assert_eq!(s.height_pt(), 874.0);
    }

    #[test]
    fn counts_available_ios_runtimes() {
        let json = r#"{"runtimes": [
            {"platform": "iOS", "identifier": "com.apple.CoreSimulator.SimRuntime.iOS-27-0", "isAvailable": true},
            {"platform": "iOS", "identifier": "com.apple.CoreSimulator.SimRuntime.iOS-18-0", "isAvailable": false},
            {"platform": "watchOS", "identifier": "com.apple.CoreSimulator.SimRuntime.watchOS-12-0", "isAvailable": true},
            {"identifier": "com.apple.CoreSimulator.SimRuntime.iOS-26-5"}
        ]}"#;
        assert_eq!(count_ios_runtimes(json).unwrap(), 2);
        assert_eq!(count_ios_runtimes(r#"{"runtimes": []}"#).unwrap(), 0);
        assert!(count_ios_runtimes("nope").is_err());
    }
}
