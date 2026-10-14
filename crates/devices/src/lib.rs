//! Simulators and emulators behind the [`Device`] trait. Headless: no UI types here.

use std::sync::Arc;
use std::time::Instant;

pub mod android;
pub mod geometry;
pub mod logs;
pub mod simctl;

pub use logs::{LogEntry, LogFilter, LogLevel, LogStream};

#[cfg(target_os = "macos")]
pub mod ios;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("unsupported: {0}")]
    Unsupported(&'static str),
    #[error("device not found: {0}")]
    NotFound(String),
    #[error("command failed: {0}")]
    Command(String),
    #[error("private API: {0}")]
    PrivateApi(String),
    #[error("permission required: {0}")]
    Permission(String),
    #[error("parse error: {0}")]
    Parse(String),
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
}

pub type Result<T, E = Error> = std::result::Result<T, E>;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Platform {
    Ios,
    Android,
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

    /// Writes the frame as an RGBA PNG (for debugging and verification).
    pub fn save_png(&self, path: &std::path::Path) -> Result<()> {
        let mut rgba = Vec::with_capacity(self.width as usize * self.height as usize * 4);
        for y in 0..self.height as usize {
            let row = &self.bgra[y * self.stride..y * self.stride + self.width as usize * 4];
            for px in row.as_chunks::<4>().0 {
                rgba.extend_from_slice(&[px[2], px[1], px[0], px[3]]);
            }
        }
        image::save_buffer(
            path,
            &rgba,
            self.width,
            self.height,
            image::ExtendedColorType::Rgba8,
        )
        .map_err(|e| Error::Parse(format!("png encode: {e}")))
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
    /// A non-text keyboard key press (text goes through [`Device::send_text`]).
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

/// Latest-frame channel: receivers always see the most recent frame; slow consumers skip frames.
pub type FrameReceiver = tokio::sync::watch::Receiver<Option<Frame>>;
pub type FrameSender = tokio::sync::watch::Sender<Option<Frame>>;

pub trait Device: Send + Sync {
    fn info(&self) -> &DeviceInfo;

    /// Short backend name for logs ("private", "simctl", "grpc", "adb").
    fn backend(&self) -> &'static str;

    fn platform(&self) -> Platform {
        Platform::Ios
    }

    fn frames(&self) -> FrameReceiver;

    fn send_input(&self, input: Input) -> Result<()>;

    /// Types text into the focused field.
    fn send_text(&self, _text: &str) -> Result<()> {
        Err(Error::Unsupported("text input"))
    }

    /// Rotates the device and returns once the guest reports the new orientation. Frames and the
    /// space of [`Input`] follow it. Unsupported on iOS (no public or reverse-engineered route
    /// works on Xcode 27, see spikes/m5-ios-notes.md).
    fn set_orientation(&self, _orientation: Orientation) -> Result<()> {
        Err(Error::Unsupported("set_orientation"))
    }

    /// The orientation the guest currently shows. Unsupported wherever `set_orientation` is.
    fn orientation(&self) -> Result<Orientation> {
        Err(Error::Unsupported("orientation"))
    }

    fn boot(&self) -> Result<()> {
        Err(Error::Unsupported("boot"))
    }

    fn shutdown(&self) -> Result<()> {
        Err(Error::Unsupported("shutdown"))
    }

    fn install(&self, _app_path: &std::path::Path) -> Result<()> {
        Err(Error::Unsupported("install"))
    }

    fn launch(&self, _bundle_id: &str) -> Result<()> {
        Err(Error::Unsupported("launch"))
    }

    /// Streams the device log. Blocking/threaded: poll the returned [`LogStream`].
    fn logs(&self, _filter: &LogFilter) -> Result<LogStream> {
        Err(Error::Unsupported("logs"))
    }
}
