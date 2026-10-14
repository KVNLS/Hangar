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
}

/// What an open device's backend can do beyond touches and Home, so callers ask instead of
/// trying and matching on `Unsupported`. A `false` flag means the matching `Device` method (or
/// input) fails with `Unsupported`; a `true` flag means it is implemented, not that it cannot fail.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Capabilities {
    /// `send_text`.
    pub text: bool,
    /// [`Input::Key`].
    pub keys: bool,
    /// [`Input::Lock`] and the volume rocker.
    pub side_buttons: bool,
    /// [`Input::Back`] and [`Input::AppSwitch`] (Android).
    pub navigation_buttons: bool,
    /// `set_orientation` and `orientation`.
    pub rotate: bool,
    pub boot: bool,
    pub shutdown: bool,
    pub install: bool,
    pub launch: bool,
    pub logs: bool,
    /// Logs from before the stream started (`LogFilter::history`).
    pub log_history: bool,
}

impl Capabilities {
    pub const ALL: Self = Self {
        text: true,
        keys: true,
        side_buttons: true,
        navigation_buttons: true,
        rotate: true,
        boot: true,
        shutdown: true,
        install: true,
        launch: true,
        logs: true,
        log_history: true,
    };

    /// What `input` needs that this device lacks, worded for an "unsupported" error.
    pub fn missing_for(&self, input: &Input) -> Option<&'static str> {
        match input {
            Input::Back | Input::AppSwitch if !self.navigation_buttons => {
                Some("Back and app switcher buttons on this device")
            }
            Input::Lock | Input::VolumeUp | Input::VolumeDown if !self.side_buttons => {
                Some("side and volume buttons on this device")
            }
            Input::Key(_) if !self.keys => Some("keyboard keys on this device"),
            _ => None,
        }
    }

    pub fn accepts(&self, input: &Input) -> bool {
        self.missing_for(input).is_none()
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

/// One frame of a device screen, `width` x `height` BGRA pixels.
#[derive(Debug, Clone)]
pub struct Frame {
    pub width: u32,
    pub height: u32,
    pub pixels: FramePixels,
    pub captured_at: Instant,
    pub seq: u64,
}

#[derive(Debug, Clone)]
pub enum FramePixels {
    /// Captured bytes, `stride` bytes per row.
    Bgra { stride: usize, data: Arc<[u8]> },
    /// A surface the backend keeps drawing into, shared with the GPU so the UI can show it without
    /// copying. Its contents are those of "now", not of the frame's `seq`.
    Live(Arc<dyn LiveSurface>),
}

pub trait LiveSurface: Send + Sync + std::fmt::Debug + std::any::Any {
    /// Copies the current contents as tightly packed BGRA (`width * 4` bytes per row), or `None`
    /// if the surface cannot be read.
    fn snapshot(&self) -> Option<Vec<u8>>;
}

impl Frame {
    /// A frame of captured bytes.
    pub fn bgra(width: u32, height: u32, stride: usize, data: Arc<[u8]>, seq: u64) -> Self {
        Self {
            width,
            height,
            pixels: FramePixels::Bgra { stride, data },
            captured_at: Instant::now(),
            seq,
        }
    }

    /// The captured bytes and their stride; `None` for a live surface (see [`Frame::snapshot`]).
    pub fn bytes(&self) -> Option<(usize, &[u8])> {
        match &self.pixels {
            FramePixels::Bgra { stride, data } => Some((*stride, data)),
            FramePixels::Live(_) => None,
        }
    }

    /// This frame with its pixels frozen: a copy of a live surface's current contents, or a cheap
    /// clone of captured bytes. `None` if the surface cannot be read.
    pub fn snapshot(&self) -> Option<Frame> {
        let FramePixels::Live(surface) = &self.pixels else {
            return Some(self.clone());
        };
        let stride = self.width as usize * 4;
        let data = surface.snapshot()?;
        if data.len() < stride * self.height as usize {
            return None;
        }
        Some(Frame {
            pixels: FramePixels::Bgra {
                stride,
                data: Arc::from(data),
            },
            ..self.clone()
        })
    }

    /// Fraction (0..=1) of sampled pixels that differ between two frames; 1.0 if sizes differ or
    /// either side is a live surface (snapshot both first).
    pub fn diff_ratio(&self, other: &Frame) -> f64 {
        if self.width != other.width || self.height != other.height {
            return 1.0;
        }
        let (Some((sa, a_px)), Some((sb, b_px))) = (self.bytes(), other.bytes()) else {
            return 1.0;
        };
        const STEP: usize = 7; // sample every 7th pixel in both directions
        let (mut total, mut changed) = (0u64, 0u64);
        for y in (0..self.height as usize).step_by(STEP) {
            for x in (0..self.width as usize).step_by(STEP) {
                let a = y * sa + x * 4;
                let b = y * sb + x * 4;
                total += 1;
                if a_px.get(a..a + 3) != b_px.get(b..b + 3) {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn capabilities_gate_inputs() {
        let none = Capabilities::default();
        assert!(none.accepts(&Input::Tap { x: 0.0, y: 0.0 }));
        assert!(none.accepts(&Input::Home));
        assert!(!none.accepts(&Input::Back));
        assert!(!none.accepts(&Input::VolumeUp));
        assert_eq!(
            none.missing_for(&Input::Key(Key::Enter)),
            Some("keyboard keys on this device")
        );
        for input in [
            Input::Back,
            Input::AppSwitch,
            Input::Lock,
            Input::Key(Key::Tab),
        ] {
            assert!(Capabilities::ALL.accepts(&input), "{input:?}");
        }
    }

    #[derive(Debug)]
    struct Surface(std::sync::Mutex<u8>);

    impl LiveSurface for Surface {
        fn snapshot(&self) -> Option<Vec<u8>> {
            let fill = *self.0.lock().ok()?;
            Some(vec![fill; 4 * 4 * 2])
        }
    }

    fn live(surface: &Arc<Surface>) -> Frame {
        Frame {
            pixels: FramePixels::Live(surface.clone()),
            ..Frame::bgra(4, 2, 16, Arc::from(vec![0u8; 32]), 1)
        }
    }

    #[test]
    fn snapshot_freezes_live_surfaces() {
        let surface = Arc::new(Surface(std::sync::Mutex::new(7)));
        let frame = live(&surface);
        assert!(frame.bytes().is_none());
        let before = frame.snapshot().unwrap();
        assert_eq!(before.bytes().unwrap(), (16, &[7u8; 32][..]));
        *surface.0.lock().unwrap() = 9;
        let after = frame.snapshot().unwrap();
        assert_eq!(after.diff_ratio(&before), 1.0);
        assert_eq!(after.diff_ratio(&after.clone()), 0.0);
        assert_eq!(before.bytes().unwrap().1[0], 7);
    }

    #[test]
    fn diff_ratio_refuses_live_frames() {
        let surface = Arc::new(Surface(std::sync::Mutex::new(0)));
        let frame = live(&surface);
        assert_eq!(frame.diff_ratio(&frame), 1.0);
    }

    #[test]
    fn short_snapshots_are_rejected() {
        let surface = Arc::new(Surface(std::sync::Mutex::new(0)));
        let frame = Frame {
            width: 100,
            ..live(&surface)
        };
        assert!(frame.snapshot().is_none());
    }
}
