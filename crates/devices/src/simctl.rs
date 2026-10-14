//! `xcrun simctl` JSON parsing (pure, testable anywhere) and invocation (macOS only).

use std::collections::BTreeMap;

use serde::Deserialize;

use crate::{DeviceInfo, Error, Result, ScreenSpec};

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
}

/// Parses `xcrun simctl list devices [booted] -j`; returns only booted devices.
pub fn parse_booted(json: &str) -> Result<Vec<DeviceInfo>> {
    let list: DeviceList = serde_json::from_str(json).map_err(|e| Error::Parse(e.to_string()))?;
    let mut out = Vec::new();
    for (runtime, devices) in list.devices {
        for d in devices.into_iter().filter(|d| d.state == "Booted") {
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

/// Lists booted simulators with their screen geometry filled in when available.
#[cfg(target_os = "macos")]
pub fn list_booted() -> Result<Vec<DeviceInfo>> {
    let json = run("xcrun", &["simctl", "list", "devices", "booted", "-j"])?;
    let mut devices = parse_booted(&String::from_utf8_lossy(&json))?;
    if devices.is_empty() {
        return Ok(devices);
    }
    let types_json = run("xcrun", &["simctl", "list", "devicetypes", "-j"])?;
    let types = parse_device_types(&String::from_utf8_lossy(&types_json))?;
    for d in &mut devices {
        let Some(bundle) = d.device_type.as_ref().and_then(|t| types.get(t)) else {
            continue;
        };
        let plist = format!("{bundle}/Contents/Resources/capabilities.plist");
        match run("plutil", &["-convert", "json", "-o", "-", &plist])
            .and_then(|j| parse_screen_spec(&String::from_utf8_lossy(&j)))
        {
            Ok(spec) => d.screen = Some(spec),
            Err(e) => tracing::warn!("no screen spec for {}: {e}", d.name),
        }
    }
    Ok(devices)
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
}
