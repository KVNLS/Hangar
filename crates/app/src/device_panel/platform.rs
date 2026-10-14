//! Device platforms the panel knows: how to list their devices, open one, and which hardware
//! controls they support. A new platform (Android) plugs in by adding a variant here; every
//! function in this module does process or framework I/O, so call them off the UI thread.

use hangar_devices::{Device, DeviceInfo, Input};
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Platform {
    Ios,
    Android,
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

const ANDROID_CONTROLS: &[Control] = &[
    Control {
        id: "back",
        label: "Back",
        tooltip: "Back button",
        input: Input::Back,
    },
    Control {
        id: "home",
        label: "Home",
        tooltip: "Home button",
        input: Input::Home,
    },
];

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
    pub const ALL: [Platform; 2] = [Platform::Ios, Platform::Android];

    pub fn label(self) -> &'static str {
        match self {
            Self::Ios => "iOS",
            Self::Android => "Android",
        }
    }

    /// Stable identifier used in picker values.
    pub fn id(self) -> &'static str {
        match self {
            Self::Ios => "ios",
            Self::Android => "android",
        }
    }

    /// Whether the device can be opened right away (booted simulator, online emulator).
    pub fn is_ready(self, info: &DeviceInfo) -> bool {
        match self {
            Self::Ios => info.state == "Booted",
            Self::Android => info.state == hangar_devices::android::state::BOOTED,
        }
    }

    pub fn controls(self) -> &'static [Control] {
        match self {
            Self::Ios => IOS_CONTROLS,
            Self::Android => ANDROID_CONTROLS,
        }
    }

    /// Whether Hangar can boot this listed device: Android AVDs that are not running.
    pub fn can_boot(self, info: &DeviceInfo) -> bool {
        matches!(self, Self::Android) && info.state == hangar_devices::android::state::SHUTDOWN
    }

    /// Whether an opened device of this platform can be shut down from Hangar: emulators only
    /// (adb serials `emulator-<port>`); physical devices cannot be powered off over adb.
    pub fn can_shutdown(self, info: &DeviceInfo) -> bool {
        matches!(self, Self::Android) && info.udid.starts_with("emulator-")
    }

    /// Shown when no device of this platform is ready. iOS boot is not available from Hangar
    /// yet (`Device::boot` returns `Unsupported`, and only booted simulators are listed).
    pub fn not_ready_hint(self) -> String {
        let unsupported = hangar_devices::Error::Unsupported("boot");
        match self {
            Self::Ios => format!(
                "Booting from Hangar is not available ({unsupported}). Boot a simulator with \
                 `xcrun simctl boot <UDID>`, then refresh."
            ),
            Self::Android => "Select an AVD and press Boot to start it from Hangar, or start an \
                 emulator or attach a device yourself, then refresh."
                .to_owned(),
        }
    }

    /// Lists this platform's devices. Blocking.
    pub fn list(self) -> anyhow::Result<Vec<DeviceInfo>> {
        match self {
            Self::Ios => list_ios(),
            Self::Android => Ok(hangar_devices::android::list()?),
        }
    }

    /// Opens a device. iOS tries the private backend first and falls back to `simctl` (frames
    /// only) when it fails; Android emulators try gRPC, then adb. `forced` (`private` or
    /// `simctl`; `grpc` or `adb`) skips the fallback. Blocking.
    pub fn open(self, info: DeviceInfo, forced: Option<&str>) -> anyhow::Result<Opened> {
        match self {
            Self::Ios => open_ios(info, forced),
            Self::Android => open_android(&info, forced),
        }
    }

    /// Boots a shut-down device and returns it as listed once it is up. Blocks for the whole
    /// boot (seconds to minutes).
    pub fn boot(self, info: &DeviceInfo) -> anyhow::Result<DeviceInfo> {
        match self {
            Self::Android => Ok(hangar_devices::android::boot(&info.udid)?),
            Self::Ios => Err(hangar_devices::Error::Unsupported("iOS boot").into()),
        }
    }
}

/// The forced backend (`HANGAR_DEVICE_BACKEND`) that applies to `platform`. The variable is
/// shared: another platform's backend name is ignored, an unknown name is kept so opening fails.
pub fn forced_for(platform: Platform, forced: Option<&str>) -> Option<&str> {
    let other: &[&str] = match platform {
        Platform::Ios => &["grpc", "adb"],
        Platform::Android => &["private", "simctl"],
    };
    forced.filter(|f| !other.contains(f))
}

fn open_android(info: &DeviceInfo, forced: Option<&str>) -> anyhow::Result<Opened> {
    use hangar_devices::android::{self, Backend};

    if let Some(name) = forced_for(Platform::Android, forced) {
        let backend: Backend = name.parse().map_err(anyhow::Error::msg)?;
        let device = match backend {
            Backend::Grpc => android::open_grpc(info)?,
            Backend::Adb => android::open_adb(info)?,
        };
        return Ok(Opened {
            device,
            reason: Some(BackendReason::Forced(format!(
                "{name} backend forced by HANGAR_DEVICE_BACKEND"
            ))),
        });
    }
    if !Platform::Android.can_shutdown(info) {
        // Physical devices only have the adb backend.
        return Ok(Opened {
            device: android::open_adb(info)?,
            reason: None,
        });
    }
    let (device, fallback) = open_with_fallback(
        "grpc",
        || Ok(android::open_grpc(info)?),
        "adb",
        || Ok(android::open_adb(info)?),
    )?;
    Ok(Opened {
        device,
        reason: fallback.map(BackendReason::Fallback),
    })
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
    if let Some(name) = forced_for(Platform::Ios, forced) {
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

    fn android_info(udid: &str, state: &str) -> DeviceInfo {
        DeviceInfo {
            udid: udid.into(),
            name: "Pixel 10 Pro".into(),
            runtime: "Android 17".into(),
            state: state.into(),
            device_type: Some("Pixel_10_Pro".into()),
            screen: None,
        }
    }

    #[test]
    fn platform_ids_and_labels() {
        assert_eq!(Platform::ALL, [Platform::Ios, Platform::Android]);
        assert_eq!(
            (Platform::Android.id(), Platform::Android.label()),
            ("android", "Android")
        );
    }

    #[test]
    fn android_readiness_boot_and_controls() {
        let a = Platform::Android;
        assert!(a.is_ready(&android_info("emulator-5554", "Booted")));
        assert!(!a.is_ready(&android_info("Pixel_10_Pro", "Shutdown")));
        assert!(!a.is_ready(&android_info("emulator-5554", "Booting")));
        assert!(a.can_boot(&android_info("Pixel_10_Pro", "Shutdown")));
        assert!(!a.can_boot(&android_info("emulator-5554", "Booted")));
        assert!(!Platform::Ios.can_boot(&info("Shutdown")));
        assert!(a.can_shutdown(&android_info("emulator-5554", "Booted")));
        assert!(!a.can_shutdown(&android_info("R5CT1234", "Booted")));
        let ids: Vec<_> = a.controls().iter().map(|c| c.id).collect();
        assert_eq!(ids, ["back", "home"]);
        assert!(a.not_ready_hint().contains("Boot"));
    }

    #[test]
    fn forced_backend_applies_per_platform() {
        assert_eq!(forced_for(Platform::Ios, Some("simctl")), Some("simctl"));
        assert_eq!(forced_for(Platform::Ios, Some("adb")), None);
        assert_eq!(forced_for(Platform::Android, Some("grpc")), Some("grpc"));
        assert_eq!(forced_for(Platform::Android, Some("private")), None);
        assert_eq!(forced_for(Platform::Android, Some("bogus")), Some("bogus"));
        assert_eq!(forced_for(Platform::Android, None), None);
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
