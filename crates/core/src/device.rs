//! Device vocabulary shared by the device adapters, the run loop, the MCP tools and the UI:
//! platforms, listed devices, frames, input and log levels. Pure data and rules, no I/O.

use std::sync::Arc;
use std::time::Instant;

/// `DeviceInfo::state` values Hangar acts on (simctl and the Android adapter use the same words).
pub mod state {
    /// Simulator booted; Android: online in adb and `sys.boot_completed=1`.
    pub const BOOTED: &str = "Booted";
    /// Android online in adb, still starting.
    pub const BOOTING: &str = "Booting";
    /// Android process running (or device attached) but adb cannot talk to it yet or any more.
    pub const OFFLINE: &str = "Offline";
    /// Simulator shut down; AVD not running.
    pub const SHUTDOWN: &str = "Shutdown";
}

#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, serde::Serialize, serde::Deserialize,
)]
#[serde(rename_all = "lowercase")]
pub enum Platform {
    Ios,
    Android,
}

impl Platform {
    pub const ALL: [Platform; 2] = [Platform::Ios, Platform::Android];

    pub fn label(self) -> &'static str {
        match self {
            Self::Ios => "iOS",
            Self::Android => "Android",
        }
    }

    /// Stable identifier (picker values, saved settings).
    pub fn id(self) -> &'static str {
        match self {
            Self::Ios => "ios",
            Self::Android => "android",
        }
    }

    /// Whether the listed device can be opened right away (booted simulator, online emulator).
    pub fn is_ready(self, info: &DeviceInfo) -> bool {
        match self {
            Self::Ios => info.state == state::BOOTED,
            Self::Android => info.state == state::BOOTED,
        }
    }

    /// Whether Hangar can boot this listed device: shut-down iOS simulators and Android AVDs
    /// that are not running.
    pub fn can_boot(self, info: &DeviceInfo) -> bool {
        match self {
            Self::Ios => info.state == state::SHUTDOWN,
            Self::Android => info.state == state::SHUTDOWN,
        }
    }

    /// Whether an opened device can be shut down from Hangar: every iOS simulator, and Android
    /// emulators only (adb serials `emulator-<port>`); physical devices cannot be powered off
    /// over adb.
    pub fn can_shutdown(self, info: &DeviceInfo) -> bool {
        match self {
            Self::Ios => true,
            Self::Android => info.udid.starts_with("emulator-"),
        }
    }

    /// Whether `Device::set_orientation` can work on this platform (not on iOS, Xcode 27).
    pub fn can_rotate(self) -> bool {
        matches!(self, Self::Android)
    }
}

/// Physical description of the main screen.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ScreenSpec {
    pub width_px: u32,
    pub height_px: u32,
    pub scale: f64,
}

impl ScreenSpec {
    pub fn width_pt(&self) -> f64 {
        f64::from(self.width_px) / self.scale
    }

    pub fn height_pt(&self) -> f64 {
        f64::from(self.height_px) / self.scale
    }

    /// The spec as the display currently shows it: width and height swapped when `landscape`
    /// differs from the spec's own shape. Specs describe the natural (portrait) panel.
    #[must_use]
    pub fn oriented(self, landscape: bool) -> Self {
        if (self.width_px > self.height_px) == landscape {
            self
        } else {
            Self {
                width_px: self.height_px,
                height_px: self.width_px,
                ..self
            }
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct DeviceInfo {
    pub udid: String,
    pub name: String,
    pub runtime: String,
    pub state: String,
    pub device_type: Option<String>,
    pub screen: Option<ScreenSpec>,
}

/// One captured frame, tightly packed BGRA (`stride == width * 4` unless stated otherwise).
#[derive(Debug, Clone)]
pub struct Frame {
    pub width: u32,
    pub height: u32,
    pub stride: usize,
    pub bgra: Arc<[u8]>,
    pub captured_at: Instant,
    pub seq: u64,
}

impl Frame {
    /// Fraction (0..=1) of sampled pixels that differ between two frames; 1.0 if sizes differ.
    pub fn diff_ratio(&self, other: &Frame) -> f64 {
        if self.width != other.width || self.height != other.height {
            return 1.0;
        }
        const STEP: usize = 7; // sample every 7th pixel in both directions
        let (mut total, mut changed) = (0u64, 0u64);
        for y in (0..self.height as usize).step_by(STEP) {
            for x in (0..self.width as usize).step_by(STEP) {
                let a = y * self.stride + x * 4;
                let b = y * other.stride + x * 4;
                total += 1;
                if self.bgra.get(a..a + 3) != other.bgra.get(b..b + 3) {
                    changed += 1;
                }
            }
        }
        if total == 0 {
            0.0
        } else {
            changed as f64 / total as f64
        }
    }
}

/// Input in device points, origin top-left of the display *as currently oriented* (the space of
/// the latest [`Frame`]: in landscape x runs along the long edge). On Android a point is a dp
/// (density-independent pixel, `px / ScreenSpec::scale`).
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Input {
    Tap {
        x: f64,
        y: f64,
    },
    TouchDown {
        x: f64,
        y: f64,
    },
    TouchMove {
        x: f64,
        y: f64,
    },
    TouchUp {
        x: f64,
        y: f64,
    },
    /// Hardware Home button press (on Face ID devices it still returns to the home screen).
    Home,
    /// Android Back (unsupported on iOS).
    Back,
    /// Side/power button press: locks the screen on iOS, toggles the screen on Android.
    Lock,
    /// Volume up rocker.
    VolumeUp,
    /// Volume down rocker.
    VolumeDown,
    /// Android app switcher / recents (unsupported on iOS).
    AppSwitch,
    /// A non-text keyboard key press (text goes through `Device::send_text`).
    Key(Key),
}

/// Keyboard keys without a printable character.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Key {
    Backspace,
    Enter,
    Tab,
    Escape,
    ArrowUp,
    ArrowDown,
    ArrowLeft,
    ArrowRight,
}

/// Physical orientation of the device (UIDeviceOrientation naming: the side the home indicator
/// or top edge points to).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Orientation {
    Portrait,
    PortraitUpsideDown,
    LandscapeLeft,
    LandscapeRight,
}

impl Orientation {
    pub fn is_landscape(self) -> bool {
        matches!(self, Self::LandscapeLeft | Self::LandscapeRight)
    }

    /// Android `Surface.ROTATION_*` index (quarter turns counter-clockwise): `LandscapeLeft`
    /// (top edge pointing left) is `ROTATION_90`.
    pub fn android_rotation(self) -> u8 {
        match self {
            Self::Portrait => 0,
            Self::LandscapeLeft => 1,
            Self::PortraitUpsideDown => 2,
            Self::LandscapeRight => 3,
        }
    }

    pub fn from_android_rotation(rotation: u8) -> Option<Self> {
        match rotation {
            0 => Some(Self::Portrait),
            1 => Some(Self::LandscapeLeft),
            2 => Some(Self::PortraitUpsideDown),
            3 => Some(Self::LandscapeRight),
            _ => None,
        }
    }

    /// The next orientation when turning the device counter-clockwise (`left`) or clockwise.
    #[must_use]
    pub fn rotated(self, left: bool) -> Self {
        let r = self.android_rotation();
        let next = if left { (r + 1) % 4 } else { (r + 3) % 4 };
        Self::from_android_rotation(next).unwrap_or(self)
    }
}

/// Severity, ordered from least to most severe. iOS `Default` maps to `Info`, Android `F` (fatal)
/// and iOS `Fault` both map to `Fault`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum LogLevel {
    Verbose,
    Debug,
    Info,
    Warn,
    Error,
    Fault,
}

impl LogLevel {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Verbose => "verbose",
            Self::Debug => "debug",
            Self::Info => "info",
            Self::Warn => "warn",
            Self::Error => "error",
            Self::Fault => "fault",
        }
    }
}

impl std::fmt::Display for LogLevel {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

impl std::str::FromStr for LogLevel {
    type Err = String;

    fn from_str(s: &str) -> std::result::Result<Self, Self::Err> {
        match s.to_ascii_lowercase().as_str() {
            "verbose" | "v" => Ok(Self::Verbose),
            "debug" | "d" => Ok(Self::Debug),
            "info" | "i" => Ok(Self::Info),
            "warn" | "warning" | "w" => Ok(Self::Warn),
            "error" | "e" => Ok(Self::Error),
            "fault" | "fatal" | "f" => Ok(Self::Fault),
            other => Err(format!(
                "unknown log level {other:?} (expected verbose|debug|info|warn|error|fault)"
            )),
        }
    }
}
