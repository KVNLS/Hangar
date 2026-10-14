//! Device platforms the panel knows: how to list their devices, open one, and which hardware
//! controls they support. A new platform (Android) plugs in by adding a variant here; every
//! function in this module does process or framework I/O, so call them off the UI thread.

use hangar_devices::{Device, DeviceInfo, Input};
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Platform {
    Ios,
}

/// A hardware control shown under the screen.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Control {
    pub id: &'static str,
    pub label: &'static str,
    pub tooltip: &'static str,
    pub input: Input,
}

const IOS_CONTROLS: &[Control] = &[Control {
    id: "home",
    label: "Home",
    tooltip: "Home button",
    input: Input::Home,
}];

/// A device opened by [`Platform::open`].
pub struct Opened {
    pub device: Box<dyn Device>,
    /// Why the default backend is not the one in use.
    pub reason: Option<BackendReason>,
}

#[derive(Clone, Debug, PartialEq)]
pub enum BackendReason {
    /// The preferred backend failed to start.
    Fallback(String),
    /// Chosen explicitly (`HANGAR_DEVICE_BACKEND`).
    Forced(String),
}

/// What [`list_all`] found, per platform.
#[derive(Debug, Default)]
pub struct Listing {
    pub devices: Vec<(Platform, DeviceInfo)>,
    pub errors: Vec<(Platform, String)>,
}

impl Platform {
    pub const ALL: [Platform; 1] = [Platform::Ios];

    pub fn label(self) -> &'static str {
        match self {
            Self::Ios => "iOS",
        }
    }

    /// Stable identifier used in picker values.
    pub fn id(self) -> &'static str {
        match self {
            Self::Ios => "ios",
        }
    }

    /// Whether the device can be opened right away (booted simulator, online emulator).
    pub fn is_ready(self, info: &DeviceInfo) -> bool {
        match self {
            Self::Ios => info.state == "Booted",
        }
    }

    pub fn controls(self) -> &'static [Control] {
        match self {
            Self::Ios => IOS_CONTROLS,
        }
    }

    /// Shown when no device of this platform is ready. Hangar cannot boot devices yet:
    /// `Device::boot` returns `Unsupported`, and only booted simulators are listed.
    pub fn not_ready_hint(self) -> String {
        let unsupported = hangar_devices::Error::Unsupported("boot");
        match self {
            Self::Ios => format!(
                "Booting from Hangar is not available ({unsupported}). Boot a simulator with \
                 `xcrun simctl boot <UDID>`, then refresh."
            ),
        }
    }

    /// Lists this platform's devices. Blocking.
    pub fn list(self) -> anyhow::Result<Vec<DeviceInfo>> {
        match self {
            Self::Ios => list_ios(),
        }
    }

    /// Opens a device. iOS tries the private backend first and falls back to `simctl` (frames
    /// only) when it fails; `forced` (`private` or `simctl`) skips the fallback. Blocking.
    pub fn open(self, info: DeviceInfo, forced: Option<&str>) -> anyhow::Result<Opened> {
        match self {
            Self::Ios => open_ios(info, forced),
        }
    }
}

/// Lists every platform's devices; one platform failing does not hide the others. Blocking.
pub fn list_all() -> Listing {
    let mut listing = Listing::default();
    for platform in Platform::ALL {
        match platform.list() {
            Ok(devices) => listing
                .devices
                .extend(devices.into_iter().map(|d| (platform, d))),
            Err(err) => listing.errors.push((platform, format!("{err:#}"))),
        }
    }
    listing
}

/// Runs `primary`; if it fails, runs `fallback` and returns why the fallback was needed.
pub fn open_with_fallback<T>(
    primary_name: &str,
    primary: impl FnOnce() -> anyhow::Result<T>,
    fallback_name: &str,
    fallback: impl FnOnce() -> anyhow::Result<T>,
) -> anyhow::Result<(T, Option<String>)> {
    match primary() {
        Ok(value) => Ok((value, None)),
        Err(primary_err) => {
            let reason = format!("{primary_name} backend failed: {primary_err:#}");
            tracing::warn!("{reason}; trying {fallback_name}");
            match fallback() {
                Ok(value) => Ok((value, Some(reason))),
                Err(fallback_err) => Err(anyhow::anyhow!(
                    "{reason}; {fallback_name} fallback failed: {fallback_err:#}"
                )),
            }
        }
    }
}

#[cfg(target_os = "macos")]
fn list_ios() -> anyhow::Result<Vec<DeviceInfo>> {
    Ok(hangar_devices::simctl::list_booted()?)
}

#[cfg(not(target_os = "macos"))]
fn list_ios() -> anyhow::Result<Vec<DeviceInfo>> {
    Err(hangar_devices::Error::Unsupported("iOS simulators need macOS").into())
}

#[cfg(target_os = "macos")]
fn open_ios(info: DeviceInfo, forced: Option<&str>) -> anyhow::Result<Opened> {
    use hangar_devices::ios::{self, Backend};

    let open =
        |backend| -> anyhow::Result<Box<dyn Device>> { Ok(ios::open(info.clone(), backend)?) };
    if let Some(name) = forced {
        let backend: Backend = name.parse().map_err(anyhow::Error::msg)?;
        return Ok(Opened {
            device: open(backend)?,
            reason: Some(BackendReason::Forced(format!(
                "{name} backend forced by HANGAR_DEVICE_BACKEND"
            ))),
        });
    }
    let (device, fallback) = open_with_fallback(
        "private",
        || open(Backend::Private),
        "simctl",
        || open(Backend::Simctl),
    )?;
    Ok(Opened {
        device,
        reason: fallback.map(BackendReason::Fallback),
    })
}

#[cfg(not(target_os = "macos"))]
fn open_ios(_: DeviceInfo, _: Option<&str>) -> anyhow::Result<Opened> {
    Err(hangar_devices::Error::Unsupported("iOS simulators need macOS").into())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn info(state: &str) -> DeviceInfo {
        DeviceInfo {
            udid: "U".into(),
            name: "iPhone 17".into(),
            runtime: "iOS 27.0".into(),
            state: state.into(),
            device_type: None,
            screen: None,
        }
    }

    #[test]
    fn fallback_only_when_primary_fails() {
        let (v, why) = open_with_fallback("a", || Ok(1), "b", || Ok(2)).unwrap();
        assert_eq!((v, why), (1, None));

        let (v, why) =
            open_with_fallback("a", || Err(anyhow::anyhow!("boom")), "b", || Ok(2)).unwrap();
        assert_eq!(v, 2);
        assert_eq!(why.as_deref(), Some("a backend failed: boom"));

        let err = open_with_fallback::<i32>(
            "a",
            || Err(anyhow::anyhow!("boom")),
            "b",
            || Err(anyhow::anyhow!("bust")),
        )
        .unwrap_err();
        assert_eq!(
            err.to_string(),
            "a backend failed: boom; b fallback failed: bust"
        );
    }

    #[test]
    fn ios_readiness_and_controls() {
        assert!(Platform::Ios.is_ready(&info("Booted")));
        assert!(!Platform::Ios.is_ready(&info("Shutdown")));
        let ids: Vec<_> = Platform::Ios.controls().iter().map(|c| c.id).collect();
        assert_eq!(ids, ["home"]);
        assert!(Platform::Ios.not_ready_hint().contains("unsupported: boot"));
    }
}
