//! What the device panel adds to [`Platform`] (defined in `hangar-devices` with its rules):
//! the hardware controls it draws, and how it lists, opens and boots devices. Listing, opening
//! and booting do process or framework I/O, so call them off the UI thread.

use gpui_kit::assets::IconName;
pub use hangar_devices::Platform;
use hangar_devices::{Device, DeviceInfo, Input};

/// A hardware control shown under the screen.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Control {
    pub id: &'static str,
    pub label: &'static str,
    pub tooltip: &'static str,
    pub icon: IconName,
    pub input: Input,
}

const HOME: Control = Control {
    id: "home",
    label: "Home",
    tooltip: "Home ⇧⌘H",
    icon: IconName::House,
    input: Input::Home,
};

const VOLUME_UP: Control = Control {
    id: "volume-up",
    label: "Vol +",
    tooltip: "Volume up",
    icon: IconName::Volume2,
    input: Input::VolumeUp,
};

const VOLUME_DOWN: Control = Control {
    id: "volume-down",
    label: "Vol \u{2212}",
    tooltip: "Volume down",
    icon: IconName::Volume1,
    input: Input::VolumeDown,
};

const IOS_CONTROLS: &[Control] = &[
    HOME,
    Control {
        id: "lock",
        label: "Lock",
        tooltip: "Side button: lock the screen",
        icon: IconName::Lock,
        input: Input::Lock,
    },
    VOLUME_UP,
    VOLUME_DOWN,
];

const ANDROID_CONTROLS: &[Control] = &[
    Control {
        id: "back",
        label: "Back",
        tooltip: "Back button",
        icon: IconName::ArrowLeft,
        input: Input::Back,
    },
    HOME,
    Control {
        id: "app-switch",
        label: "Apps",
        tooltip: "App switcher (recents)",
        icon: IconName::LayoutGrid,
        input: Input::AppSwitch,
    },
    Control {
        id: "lock",
        label: "Power",
        tooltip: "Power button: toggle the screen",
        icon: IconName::Power,
        input: Input::Lock,
    },
    VOLUME_UP,
    VOLUME_DOWN,
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

/// The panel's view of a platform: controls and hints it shows.
pub trait PlatformUi {
    fn controls(self) -> &'static [Control];
    /// Shown when no device of this platform is ready.
    fn not_ready_hint(self) -> String;
}

impl PlatformUi for Platform {
    fn controls(self) -> &'static [Control] {
        match self {
            Self::Ios => IOS_CONTROLS,
            Self::Android => ANDROID_CONTROLS,
        }
    }

    fn not_ready_hint(self) -> String {
        match self {
            Self::Ios => "Select a simulator and press Boot to start it from Hangar, or boot \
                 one yourself (`xcrun simctl boot <UDID>`), then refresh."
                .to_owned(),
            Self::Android => "Select an AVD and press Boot to start it from Hangar, or start an \
                 emulator or attach a device yourself, then refresh."
                .to_owned(),
        }
    }
}

/// Blocking device I/O per platform.
pub trait PlatformDevices {
    /// Lists this platform's devices. Blocking.
    fn list(self) -> anyhow::Result<Vec<DeviceInfo>>;

    /// Opens a device. iOS tries the private backend first and falls back to `simctl` (frames
    /// only) when it fails; Android emulators try gRPC, then adb. `forced` (`private` or
    /// `simctl`; `grpc` or `adb`) skips the fallback. Blocking.
    fn open(self, info: DeviceInfo, forced: Option<&str>) -> anyhow::Result<Opened>;

    /// Boots a shut-down device and returns it as listed once it is up, reporting each stage
    /// (`booting Android`) to `progress`. Blocks for the whole boot (seconds to minutes).
    fn boot(self, info: &DeviceInfo, progress: &dyn Fn(String)) -> anyhow::Result<DeviceInfo>;
}

impl PlatformDevices for Platform {
    fn list(self) -> anyhow::Result<Vec<DeviceInfo>> {
        match self {
            Self::Ios => list_ios(),
            Self::Android => Ok(hangar_devices::android::list()?),
        }
    }

    fn open(self, info: DeviceInfo, forced: Option<&str>) -> anyhow::Result<Opened> {
        match self {
            Self::Ios => open_ios(info, forced),
            Self::Android => open_android(&info, forced),
        }
    }

    fn boot(self, info: &DeviceInfo, progress: &dyn Fn(String)) -> anyhow::Result<DeviceInfo> {
        match self {
            Self::Android => Ok(hangar_devices::android::boot_with_progress(
                &info.udid,
                &|stage| progress(stage.to_string()),
            )?),
            Self::Ios => {
                // `simctl boot` reports nothing until it returns.
                progress("booting the simulator".to_owned());
                boot_ios(info)
            }
        }
    }
}

/// Orders simulators for the picker: booted first, then newest runtime first; `simctl` order
/// is kept within a runtime (phones before tablets).
pub fn sort_ios(devices: &mut [DeviceInfo]) {
    devices.sort_by_key(|d| {
        (
            d.state != hangar_devices::simctl::state::BOOTED,
            std::cmp::Reverse(runtime_version(&d.runtime)),
        )
    });
}

/// Numeric components of a runtime name (`iOS 26.5` -> `[26, 5]`).
fn runtime_version(runtime: &str) -> Vec<u32> {
    runtime
        .rsplit(' ')
        .next()
        .unwrap_or_default()
        .split('.')
        .map_while(|part| part.parse().ok())
        .collect()
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
    let mut devices = hangar_devices::simctl::list_all()?;
    // tvOS, watchOS and visionOS simulators have no phone-style input to drive.
    devices.retain(|d| d.runtime.starts_with("iOS"));
    sort_ios(&mut devices);
    Ok(devices)
}

#[cfg(target_os = "macos")]
fn boot_ios(info: &DeviceInfo) -> anyhow::Result<DeviceInfo> {
    hangar_devices::ios::boot(&info.udid)?;
    Ok(DeviceInfo {
        state: hangar_devices::simctl::state::BOOTED.to_owned(),
        ..info.clone()
    })
}

#[cfg(not(target_os = "macos"))]
fn boot_ios(_: &DeviceInfo) -> anyhow::Result<DeviceInfo> {
    Err(hangar_devices::Error::Unsupported("iOS simulators need macOS").into())
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
        assert!(Platform::Ios.can_boot(&info("Shutdown")));
        assert!(!Platform::Ios.can_boot(&info("Booted")));
        assert!(!Platform::Ios.can_boot(&info("Booting")));
        assert!(a.can_shutdown(&android_info("emulator-5554", "Booted")));
        assert!(!a.can_shutdown(&android_info("R5CT1234", "Booted")));
        let ids: Vec<_> = a.controls().iter().map(|c| c.id).collect();
        assert_eq!(
            ids,
            [
                "back",
                "home",
                "app-switch",
                "lock",
                "volume-up",
                "volume-down"
            ]
        );
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
        assert_eq!(ids, ["home", "lock", "volume-up", "volume-down"]);
        assert!(Platform::Ios.not_ready_hint().contains("Boot"));
        assert!(Platform::Ios.can_shutdown(&info("Booted")));
    }

    #[test]
    fn ios_list_sorts_booted_then_newest_runtime() {
        let sim = |name: &str, runtime: &str, state: &str| DeviceInfo {
            name: name.into(),
            runtime: runtime.into(),
            ..info(state)
        };
        let mut devices = vec![
            sim("a", "iOS 26.5", "Shutdown"),
            sim("b", "iOS 9.3", "Shutdown"),
            sim("c", "iOS 27.0", "Shutdown"),
            sim("d", "iOS 26.5", "Booted"),
            sim("e", "iOS 27.0", "Shutdown"),
        ];
        sort_ios(&mut devices);
        let names: Vec<_> = devices.iter().map(|d| d.name.as_str()).collect();
        assert_eq!(names, ["d", "c", "e", "a", "b"]);
    }
}
