use hangar_core::device::{Orientation, Platform};
use serde_json::json;

use super::Sharing;
use crate::screenshot::Screenshot;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Content {
    Text(String),
    /// PNG-encoded bytes (base64-encoded on the wire).
    Png(Vec<u8>),
}

/// A successful tool result.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ToolOutput {
    pub content: Vec<Content>,
}

/// The user's answer to `ask_user`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Answer {
    pub text: String,
    /// Index of the offered choice the user picked; `None` for a typed answer.
    pub choice: Option<usize>,
}

impl ToolOutput {
    pub fn text(text: impl Into<String>) -> Self {
        Self {
            content: vec![Content::Text(text.into())],
        }
    }

    /// `{"answer": "...", "choice": 1 | null}`.
    pub fn answer(answer: &Answer) -> Self {
        Self::text(json!({ "answer": answer.text, "choice": answer.choice }).to_string())
    }

    /// The image plus a line mapping its pixels to the device's point space.
    pub fn screenshot(shot: Screenshot, width_pt: f64, height_pt: f64) -> Self {
        let caption = format!(
            "Screenshot {}x{} px. Device screen is {} x {} points (origin top-left, current \
             orientation); tap at image pixel (px, py) with x = px * {:.4}, y = py * {:.4}.",
            shot.width,
            shot.height,
            fmt_points(width_pt),
            fmt_points(height_pt),
            width_pt / f64::from(shot.width.max(1)),
            height_pt / f64::from(shot.height.max(1)),
        );
        Self {
            content: vec![Content::Png(shot.png), Content::Text(caption)],
        }
    }

    /// The canonical `device_info` result (pretty JSON text).
    pub fn device_status(status: &DeviceStatus) -> Self {
        let platform = match status.platform {
            Platform::Ios => "ios",
            Platform::Android => "android",
        };
        let orientation = status.orientation.map(|o| match o {
            Orientation::Portrait => "portrait",
            Orientation::PortraitUpsideDown => "portrait_upside_down",
            Orientation::LandscapeLeft => "landscape_left",
            Orientation::LandscapeRight => "landscape_right",
        });
        let screen = status
            .screen_pt
            .map(|(w, h)| json!({ "width_pt": w, "height_pt": h, "scale": status.scale }));
        let value = json!({
            "platform": platform,
            "name": status.name,
            "id": status.id,
            "booted": status.booted,
            "screen": screen,
            "orientation": orientation,
            "platforms": status.platforms.iter().map(|p| p.id()).collect::<Vec<_>>(),
            "sharing": Self::sharing_json(status.sharing.as_ref()),
        });
        Self::text(serde_json::to_string_pretty(&value).unwrap_or_else(|_| value.to_string()))
    }
}

fn fmt_points(v: f64) -> String {
    if v.fract() == 0.0 {
        format!("{v:.0}")
    } else {
        format!("{v:.1}")
    }
}

/// Facts for `device_info`; `None` where the device cannot tell (e.g. not booted).
#[derive(Debug, Clone, PartialEq)]
pub struct DeviceStatus {
    pub platform: Platform,
    pub name: String,
    pub id: String,
    pub booted: bool,
    /// Width and height in points, as currently oriented.
    pub screen_pt: Option<(f64, f64)>,
    pub scale: Option<f64>,
    pub orientation: Option<Orientation>,
    /// Platforms this workspace may switch to with `device_acquire`.
    pub platforms: Vec<Platform>,
    pub sharing: Option<Sharing>,
}

/// A failed tool call.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ToolError {
    /// The session does not belong to an open workspace (JSON-RPC error `-32001`).
    #[error("unknown workspace session")]
    UnknownSession,
    /// The tool cannot work on this device or platform (result with `isError: true`).
    #[error("unsupported: {0}")]
    Unsupported(String),
    /// The tool ran and failed, e.g. no device is booted (result with `isError: true`).
    #[error("{0}")]
    Failed(String),
}
