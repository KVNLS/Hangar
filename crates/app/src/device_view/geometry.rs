//! Pure geometry and gesture logic of the device view: where the frame is painted, how a
//! pointer position maps to device points, and how mouse presses become touch input.
//!
//! All view coordinates are GPUI logical pixels in window space; `scale` is the window's
//! backing scale factor (2.0 on Retina).

use std::time::Duration;

use crate::adapters::devices::geometry;
use hangar_core::device::Input;

/// An axis-aligned rectangle in window space.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Rect {
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
}

impl Rect {
    pub fn is_empty(&self) -> bool {
        self.width <= 0.0 || self.height <= 0.0
    }
}

/// Where a frame of `content` pixels is painted inside `bounds`: aspect-fit, centred, the rest
/// letterboxed. Edges are snapped to the physical pixel grid so the image is not resampled
/// across a half pixel on HiDPI screens; clicks are mapped against the same snapped rect.
pub fn image_rect(bounds: Rect, content_w: f64, content_h: f64, scale: f64) -> Rect {
    let fit = geometry::contain(bounds.width, bounds.height, content_w, content_h);
    if fit.width <= 0.0 || fit.height <= 0.0 {
        return Rect {
            x: bounds.x,
            y: bounds.y,
            width: 0.0,
            height: 0.0,
        };
    }
    let scale = if scale.is_finite() && scale > 0.0 {
        scale
    } else {
        1.0
    };
    let snap = |v: f64| (v * scale).round() / scale;
    let (x0, y0) = (snap(bounds.x + fit.x), snap(bounds.y + fit.y));
    let (x1, y1) = (
        snap(bounds.x + fit.x + fit.width),
        snap(bounds.y + fit.y + fit.height),
    );
    Rect {
        x: x0,
        y: y0,
        width: (x1 - x0).max(0.0),
        height: (y1 - y0).max(0.0),
    }
}

/// Maps a window-space point over the painted image to device points. `None` in the letterbox.
pub fn to_device(px: f64, py: f64, image: Rect, screen: (f64, f64)) -> Option<(f64, f64)> {
    if image.is_empty() {
        return None;
    }
    let fx = (px - image.x) / image.width;
    let fy = (py - image.y) / image.height;
    if !(0.0..=1.0).contains(&fx) || !(0.0..=1.0).contains(&fy) {
        return None;
    }
    Some((fx * screen.0, fy * screen.1))
}

/// Like [`to_device`] but clamps to the screen edge, for drags that leave the image.
pub fn to_device_clamped(px: f64, py: f64, image: Rect, screen: (f64, f64)) -> Option<(f64, f64)> {
    if image.is_empty() {
        return None;
    }
    let fx = ((px - image.x) / image.width).clamp(0.0, 1.0);
    let fy = ((py - image.y) / image.height).clamp(0.0, 1.0);
    Some((fx * screen.0, fy * screen.1))
}

/// Size of the device screen in points (the unit of [`Input`]), in the shape of the current
/// frame: the screen spec describes the portrait panel, so a landscape frame swaps its sides.
/// Without a spec the frame's pixels are taken as points (scale 1).
pub fn screen_points(
    spec: Option<hangar_core::device::ScreenSpec>,
    frame: Option<(u32, u32)>,
) -> Option<(f64, f64)> {
    if let Some(s) = spec {
        let s = match frame.filter(|(w, h)| *w > 0 && *h > 0) {
            Some((w, h)) => s.oriented(w > h),
            None => s,
        };
        return Some((s.width_pt(), s.height_pt()));
    }
    frame
        .filter(|(w, h)| *w > 0 && *h > 0)
        .map(|(w, h)| (f64::from(w), f64::from(h)))
}

/// Pointer travel (logical pixels) below which a press is still a tap.
pub const TAP_SLOP: f64 = 4.0;
/// A press held this long without moving becomes a touch, so the device sees a long press.
pub const LONG_PRESS: Duration = Duration::from_millis(300);

/// Turns one mouse press into either a single `Tap` or a `TouchDown`/`TouchMove`/`TouchUp`
/// sequence (swipes, long presses).
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub enum Gesture {
    #[default]
    Idle,
    /// Button down, nothing sent yet.
    Pressed {
        view: (f64, f64),
        device: (f64, f64),
    },
    /// A touch is down on the device.
    Touching { last: (f64, f64) },
}

impl Gesture {
    /// Mouse down at `view`; `device` is `None` in the letterbox (the press is ignored).
    pub fn press(&mut self, view: (f64, f64), device: Option<(f64, f64)>) -> Vec<Input> {
        let out = self.cancel();
        if let Some(device) = device {
            *self = Self::Pressed { view, device };
        }
        out
    }

    /// Pointer moved while the button is down; `device` is the clamped device point.
    pub fn moved(&mut self, view: (f64, f64), device: (f64, f64)) -> Vec<Input> {
        match *self {
            Self::Idle => Vec::new(),
            Self::Pressed {
                view: start,
                device: down,
            } => {
                let (dx, dy) = (view.0 - start.0, view.1 - start.1);
                if dx.hypot(dy) < TAP_SLOP {
                    return Vec::new();
                }
                *self = Self::Touching { last: device };
                vec![
                    Input::TouchDown {
                        x: down.0,
                        y: down.1,
                    },
                    Input::TouchMove {
                        x: device.0,
                        y: device.1,
                    },
                ]
            }
            Self::Touching { last } => {
                if last == device {
                    return Vec::new();
                }
                *self = Self::Touching { last: device };
                vec![Input::TouchMove {
                    x: device.0,
                    y: device.1,
                }]
            }
        }
    }

    /// The press was held for [`LONG_PRESS`] without moving.
    pub fn hold_elapsed(&mut self) -> Vec<Input> {
        match *self {
            Self::Pressed { device, .. } => {
                *self = Self::Touching { last: device };
                vec![Input::TouchDown {
                    x: device.0,
                    y: device.1,
                }]
            }
            _ => Vec::new(),
        }
    }

    /// Mouse up; `device` is the clamped device point of the release, if the image is known.
    pub fn release(&mut self, device: Option<(f64, f64)>) -> Vec<Input> {
        let out = match *self {
            Self::Idle => Vec::new(),
            Self::Pressed { device: down, .. } => vec![Input::Tap {
                x: down.0,
                y: down.1,
            }],
            Self::Touching { last } => {
                let (x, y) = device.unwrap_or(last);
                vec![Input::TouchUp { x, y }]
            }
        };
        *self = Self::Idle;
        out
    }

    /// Ends whatever is in progress without a tap (lifts a touch that is down).
    pub fn cancel(&mut self) -> Vec<Input> {
        let out = match *self {
            Self::Touching { last } => vec![Input::TouchUp {
                x: last.0,
                y: last.1,
            }],
            _ => Vec::new(),
        };
        *self = Self::Idle;
        out
    }

    pub fn is_pressed(&self) -> bool {
        matches!(self, Self::Pressed { .. })
    }
}

#[cfg(test)]
mod tests;
