//! What the device panel adds to [`Platform`] (defined in `hangar-core` with its rules): the
//! hardware controls it draws and its hints. Listing, opening and booting go through
//! [`crate::adapters::device_catalog`] off the UI thread.

use gpui_kit::assets::IconName;
pub use hangar_core::device::{BackendReason, Platform};
use hangar_devices::{DeviceInfo, Input};

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

/// Lists every platform's devices; one platform failing does not hide the others. Blocking.
pub fn list_all() -> Listing {
    let mut listing = Listing::default();
    for platform in Platform::ALL {
        match crate::adapters::device_catalog(platform).list() {
            Ok(devices) => listing
                .devices
                .extend(devices.into_iter().map(|d| (platform, d))),
            Err(err) => listing
                .errors
                .push((platform, format!("{:#}", anyhow::Error::from(err)))),
        }
    }
    listing
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
    fn ios_readiness_and_controls() {
        assert!(Platform::Ios.is_ready(&info("Booted")));
        assert!(!Platform::Ios.is_ready(&info("Shutdown")));
        let ids: Vec<_> = Platform::Ios.controls().iter().map(|c| c.id).collect();
        assert_eq!(ids, ["home", "lock", "volume-up", "volume-down"]);
        assert!(Platform::Ios.not_ready_hint().contains("Boot"));
        assert!(Platform::Ios.can_shutdown(&info("Booted")));
    }
}
