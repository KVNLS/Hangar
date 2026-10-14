//! What [`crate::ports::DeviceCatalog`] speaks: which device to bring up for the pool, what
//! came up, and what runs. Pure data.

use super::{DeviceInfo, Platform};

/// Prefix of the simulators the pool creates (recognisable and cleanable in Device Hub).
pub const CLONE_PREFIX: &str = "Hangar · ";

/// `Hangar · <project>/<slug>`.
pub fn clone_name(project: &str, slug: &str) -> String {
    format!("{CLONE_PREFIX}{project}/{slug}")
}

/// Identifies a device across launches (saved in settings).
#[derive(Clone, Debug, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub struct DeviceKey {
    pub platform: Platform,
    pub id: String,
}

impl DeviceKey {
    /// Value used by the picker's select (`ios:<UDID>`).
    pub fn value(&self) -> String {
        format!("{}:{}", self.platform.id(), self.id)
    }
}

/// What to bring up for one pool device.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Target {
    /// A simulator by UDID.
    Ios { udid: String },
    /// A workspace's own simulator: `udid` once created; (re)created like `base`, named `name`,
    /// when it does not exist.
    IosOwn {
        udid: Option<String>,
        base: String,
        name: String,
    },
    /// The AVD's instance (any) or a new read-only one. `serial` is the instance booted for this
    /// device earlier, used when it still runs.
    Android {
        avd: String,
        serial: Option<String>,
        /// Shared base device: any running instance of the AVD will do.
        shared: bool,
    },
}

impl Target {
    pub fn platform(&self) -> Platform {
        match self {
            Self::Ios { .. } | Self::IosOwn { .. } => Platform::Ios,
            Self::Android { .. } => Platform::Android,
        }
    }
}

/// A running device found or brought up, ready to open.
#[derive(Clone, Debug, PartialEq)]
pub struct Running {
    pub info: DeviceInfo,
    /// UDID of a simulator created for this device.
    pub created: Option<String>,
}

/// The base AVD runs outside the pool without `-read-only`, which keeps the emulator from
/// starting any other instance of it.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error(
    "{avd} is already running as {serial} without -read-only, so Hangar cannot start another \
     instance of it. Shut it down (Device tab: Shut down and retry), then try again."
)]
pub struct Conflict {
    pub avd: String,
    pub serial: String,
}

/// Why the default backend is not the one in use.
#[derive(Clone, Debug, PartialEq)]
pub enum BackendReason {
    /// The preferred backend failed to start.
    Fallback(String),
    /// Chosen explicitly (`HANGAR_DEVICE_BACKEND`).
    Forced(String),
}

/// A device opened by [`crate::ports::DeviceCatalog::open`].
pub struct Opened<D> {
    pub device: D,
    /// Why the default backend is not the one in use.
    pub reason: Option<BackendReason>,
}

/// A device running now that the pool may have booted: a booted simulator (`base: None`), or a
/// read-only emulator instance (the only kind the pool boots) with its AVD.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RunningInstance {
    /// Simulator UDID or adb serial.
    pub id: String,
    pub base: Option<String>,
}

/// What runs right now, on every platform.
#[derive(Debug, Default)]
pub struct RunningSet {
    instances: Vec<(Platform, RunningInstance)>,
}

impl RunningSet {
    pub fn add(&mut self, platform: Platform, instances: Vec<RunningInstance>) {
        self.instances
            .extend(instances.into_iter().map(|i| (platform, i)));
    }

    /// The id of the target's running device, if it runs (a pool instance for Android).
    pub fn find(&self, target: &Target) -> Option<String> {
        let running = |platform: Platform, id: &str, base: Option<&str>| {
            self.instances
                .iter()
                .any(|(p, i)| *p == platform && i.id == id && i.base.as_deref() == base)
        };
        match target {
            Target::Ios { udid }
            | Target::IosOwn {
                udid: Some(udid), ..
            } => running(Platform::Ios, udid, None).then(|| udid.clone()),
            Target::Android {
                avd,
                serial: Some(serial),
                ..
            } => running(Platform::Android, serial, Some(avd)).then(|| serial.clone()),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clone_names_are_prefixed() {
        assert_eq!(clone_name("app", "fix-login"), "Hangar · app/fix-login");
        assert!(clone_name("a", "b").starts_with(CLONE_PREFIX));
    }

    #[test]
    fn running_set_matches_the_target() {
        let mut set = RunningSet::default();
        set.add(
            Platform::Ios,
            vec![RunningInstance {
                id: "U1".into(),
                base: None,
            }],
        );
        set.add(
            Platform::Android,
            vec![RunningInstance {
                id: "emulator-5556".into(),
                base: Some("Pixel".into()),
            }],
        );
        let ios = Target::Ios { udid: "U1".into() };
        assert_eq!(set.find(&ios).as_deref(), Some("U1"));
        let own = Target::IosOwn {
            udid: None,
            base: "B".into(),
            name: "n".into(),
        };
        assert_eq!(set.find(&own), None);
        let android = |avd: &str, serial: Option<&str>| Target::Android {
            avd: avd.into(),
            serial: serial.map(Into::into),
            shared: false,
        };
        assert_eq!(
            set.find(&android("Pixel", Some("emulator-5556")))
                .as_deref(),
            Some("emulator-5556")
        );
        assert_eq!(set.find(&android("Small", Some("emulator-5556"))), None);
        assert_eq!(set.find(&android("Pixel", None)), None);
        assert_eq!(ios.platform(), Platform::Ios);
    }

    #[test]
    fn conflict_explains_the_way_out() {
        let c = Conflict {
            avd: "Pixel".into(),
            serial: "emulator-5554".into(),
        };
        assert!(
            c.to_string()
                .starts_with("Pixel is already running as emulator-5554 without -read-only")
        );
    }
}
