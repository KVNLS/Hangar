//! Simulators and emulators behind the [`Device`] trait. Headless: no UI types here.

pub mod android;
pub mod capacity;
pub mod contract;
pub mod geometry;
pub mod logs;
pub mod simctl;

pub use logs::{LogEntry, LogFilter, LogStream};

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

pub use hangar_core::device::{
    Capabilities, DeviceInfo, Frame, Input, Key, LogLevel, Orientation, Platform, ScreenSpec,
};

/// PNG export of a [`Frame`], kept here because it needs the `image` encoder.
pub trait SavePng {
    /// Writes the frame as an RGBA PNG (for debugging and verification).
    fn save_png(&self, path: &std::path::Path) -> Result<()>;
}

impl SavePng for Frame {
    fn save_png(&self, path: &std::path::Path) -> Result<()> {
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

    /// What this backend implements; the default methods below implement nothing. Every
    /// backend declares its own, checked by [`contract::violations`].
    fn capabilities(&self) -> Capabilities {
        Capabilities::default()
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
