//! Pure geometry and gesture logic of the device view: where the frame is painted, how a
//! pointer position maps to device points, and how mouse presses become touch input.
//!
//! All view coordinates are GPUI logical pixels in window space; `scale` is the window's
//! backing scale factor (2.0 on Retina).

use std::time::Duration;

use hangar_devices::{Input, geometry};

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

/// Size of the device screen in points (the unit of [`Input`]). Uses the device's screen spec
/// when known; otherwise the frame's pixels are taken as points (scale 1).
pub fn screen_points(
    spec: Option<hangar_devices::ScreenSpec>,
    frame: Option<(u32, u32)>,
) -> Option<(f64, f64)> {
    if let Some(s) = spec {
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
mod tests {
    use super::*;

    fn close(a: f64, b: f64) -> bool {
        (a - b).abs() < 1e-9
    }

    fn bounds(x: f64, y: f64, width: f64, height: f64) -> Rect {
        Rect {
            x,
            y,
            width,
            height,
        }
    }

    #[test]
    fn image_is_fit_and_letterboxed_horizontally() {
        // 1206x2622 px frame in a 1000x874 panel at (100, 50): full height, centred.
        let r = image_rect(bounds(100.0, 50.0, 1000.0, 874.0), 1206.0, 2622.0, 1.0);
        assert!(close(r.height, 874.0));
        assert!(close(r.y, 50.0));
        // 402 wide, centred, then snapped to whole pixels at scale 1.
        assert!(close(r.width, 402.0));
        assert!(close(r.x, 399.0));
    }

    #[test]
    fn image_is_fit_and_letterboxed_vertically() {
        let r = image_rect(bounds(0.0, 0.0, 201.0, 1000.0), 402.0, 874.0, 1.0);
        assert!(close(r.width, 201.0));
        assert!(close(r.height, 437.0));
        assert!(close(r.y, 282.0)); // (1000 - 437) / 2 = 281.5, snapped
        assert!(close(r.x, 0.0));
    }

    #[test]
    fn image_edges_snap_to_physical_pixels() {
        // Fractional origin and size: every edge lands on a 0.5 pt grid at 2x, 1/3 at 3x.
        for scale in [1.0, 2.0, 3.0] {
            let r = image_rect(bounds(10.3, 7.7, 333.3, 500.1), 1206.0, 2622.0, scale);
            for edge in [r.x, r.y, r.x + r.width, r.y + r.height] {
                let px = edge * scale;
                assert!((px - px.round()).abs() < 1e-6, "{edge} at {scale}x");
            }
            // Snapping moves edges by at most half a physical pixel.
            let fit = geometry::contain(333.3, 500.1, 1206.0, 2622.0);
            assert!((r.width - fit.width).abs() <= 1.0 / scale + 1e-9);
        }
    }

    #[test]
    fn image_rect_degenerate() {
        assert!(image_rect(bounds(5.0, 5.0, 0.0, 100.0), 10.0, 10.0, 2.0).is_empty());
        assert!(image_rect(bounds(5.0, 5.0, 100.0, 100.0), 0.0, 10.0, 2.0).is_empty());
        // A broken scale factor falls back to 1.
        let r = image_rect(bounds(0.0, 0.0, 100.0, 100.0), 10.0, 10.0, f64::NAN);
        assert!(close(r.width, 100.0));
    }

    #[test]
    fn maps_window_points_to_device_points() {
        let screen = (402.0, 874.0);
        let image = bounds(399.0, 50.0, 402.0, 874.0);
        assert_eq!(to_device(399.0, 50.0, image, screen), Some((0.0, 0.0)));
        let (x, y) = to_device(600.0, 487.0, image, screen).unwrap();
        assert!(close(x, 201.0) && close(y, 437.0));
        let (x, y) = to_device(801.0, 924.0, image, screen).unwrap();
        assert!(close(x, 402.0) && close(y, 874.0));
        // Letterbox and outside.
        assert_eq!(to_device(398.0, 400.0, image, screen), None);
        assert_eq!(to_device(500.0, 925.0, image, screen), None);
        assert_eq!(
            to_device(
                1.0,
                1.0,
                Rect {
                    width: 0.0,
                    ..image
                },
                screen
            ),
            None
        );
    }

    #[test]
    fn mapping_is_independent_of_scale_factor() {
        // Logical pixels are the same on 1x and 2x screens: a half-size view maps to the same
        // device point whatever the backing scale.
        let screen = (402.0, 874.0);
        for scale in [1.0, 2.0, 3.0] {
            let image = image_rect(bounds(0.0, 0.0, 201.0, 437.0), 1206.0, 2622.0, scale);
            let (x, y) = to_device(100.5, 218.5, image, screen).unwrap();
            assert!(close(x, 201.0) && close(y, 437.0), "{scale}x");
        }
    }

    #[test]
    fn clamped_mapping_for_drags() {
        let screen = (402.0, 874.0);
        let image = bounds(100.0, 0.0, 201.0, 437.0);
        assert_eq!(
            to_device_clamped(0.0, -50.0, image, screen),
            Some((0.0, 0.0))
        );
        assert_eq!(
            to_device_clamped(1000.0, 1000.0, image, screen),
            Some((402.0, 874.0))
        );
    }

    #[test]
    fn screen_points_prefer_the_spec() {
        let spec = hangar_devices::ScreenSpec {
            width_px: 1206,
            height_px: 2622,
            scale: 3.0,
        };
        assert_eq!(
            screen_points(Some(spec), Some((10, 10))),
            Some((402.0, 874.0))
        );
        assert_eq!(
            screen_points(None, Some((1080, 2400))),
            Some((1080.0, 2400.0))
        );
        assert_eq!(screen_points(None, Some((0, 2400))), None);
        assert_eq!(screen_points(None, None), None);
    }

    #[test]
    fn click_is_a_tap() {
        let mut g = Gesture::default();
        assert!(g.press((10.0, 10.0), Some((30.0, 40.0))).is_empty());
        // Jitter under the slop does not start a touch.
        assert!(g.moved((12.0, 11.0), (36.0, 43.0)).is_empty());
        assert_eq!(
            g.release(Some((36.0, 43.0))),
            [Input::Tap { x: 30.0, y: 40.0 }]
        );
        assert_eq!(g, Gesture::Idle);
    }

    #[test]
    fn drag_is_a_swipe() {
        let mut g = Gesture::default();
        g.press((10.0, 10.0), Some((30.0, 40.0)));
        assert_eq!(
            g.moved((30.0, 10.0), (90.0, 40.0)),
            [
                Input::TouchDown { x: 30.0, y: 40.0 },
                Input::TouchMove { x: 90.0, y: 40.0 }
            ]
        );
        assert_eq!(
            g.moved((40.0, 10.0), (120.0, 40.0)),
            [Input::TouchMove { x: 120.0, y: 40.0 }]
        );
        // Same device point again: nothing to send.
        assert!(g.moved((40.2, 10.0), (120.0, 40.0)).is_empty());
        assert_eq!(
            g.release(Some((150.0, 40.0))),
            [Input::TouchUp { x: 150.0, y: 40.0 }]
        );
        assert!(g.release(None).is_empty());
    }

    #[test]
    fn long_press_holds_a_touch() {
        let mut g = Gesture::default();
        g.press((10.0, 10.0), Some((30.0, 40.0)));
        assert!(g.is_pressed());
        assert_eq!(g.hold_elapsed(), [Input::TouchDown { x: 30.0, y: 40.0 }]);
        assert!(g.hold_elapsed().is_empty());
        // Release without a known point lifts where the touch is.
        assert_eq!(g.release(None), [Input::TouchUp { x: 30.0, y: 40.0 }]);
    }

    #[test]
    fn letterbox_press_is_ignored() {
        let mut g = Gesture::default();
        assert!(g.press((1.0, 1.0), None).is_empty());
        assert!(g.moved((50.0, 50.0), (10.0, 10.0)).is_empty());
        assert!(g.hold_elapsed().is_empty());
        assert!(g.release(Some((10.0, 10.0))).is_empty());
    }

    #[test]
    fn new_press_lifts_a_stuck_touch() {
        let mut g = Gesture::default();
        g.press((0.0, 0.0), Some((1.0, 1.0)));
        g.hold_elapsed();
        // The release was lost (e.g. outside the window); the next press cleans up first.
        assert_eq!(
            g.press((5.0, 5.0), Some((2.0, 2.0))),
            [Input::TouchUp { x: 1.0, y: 1.0 }]
        );
        assert!(g.is_pressed());
        assert!(Gesture::default().cancel().is_empty());
    }
}
