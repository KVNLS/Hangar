//! Coordinate mapping between a view showing the device screen and device points.

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Rect {
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
}

/// Largest rect with the content's aspect ratio that fits in the container, centred
/// (CSS `object-fit: contain`).
pub fn contain(container_w: f64, container_h: f64, content_w: f64, content_h: f64) -> Rect {
    if container_w <= 0.0 || container_h <= 0.0 || content_w <= 0.0 || content_h <= 0.0 {
        return Rect {
            x: 0.0,
            y: 0.0,
            width: 0.0,
            height: 0.0,
        };
    }
    let scale = (container_w / content_w).min(container_h / content_h);
    let width = content_w * scale;
    let height = content_h * scale;
    Rect {
        x: (container_w - width) / 2.0,
        y: (container_h - height) / 2.0,
        width,
        height,
    }
}

/// Maps a point inside a view (relative to the view's origin) that shows the device screen with
/// `contain` fitting into device points. Returns `None` when the point is in the letterbox area.
pub fn view_to_device(
    view_x: f64,
    view_y: f64,
    view_w: f64,
    view_h: f64,
    screen_w_pt: f64,
    screen_h_pt: f64,
) -> Option<(f64, f64)> {
    let r = contain(view_w, view_h, screen_w_pt, screen_h_pt);
    if r.width <= 0.0 {
        return None;
    }
    let fx = (view_x - r.x) / r.width;
    let fy = (view_y - r.y) / r.height;
    if !(0.0..=1.0).contains(&fx) || !(0.0..=1.0).contains(&fy) {
        return None;
    }
    Some((fx * screen_w_pt, fy * screen_h_pt))
}

/// Device points → 0..1 fractions of the screen, clamped.
pub fn device_to_unit(x: f64, y: f64, screen_w_pt: f64, screen_h_pt: f64) -> (f64, f64) {
    let fx = if screen_w_pt > 0.0 {
        x / screen_w_pt
    } else {
        0.0
    };
    let fy = if screen_h_pt > 0.0 {
        y / screen_h_pt
    } else {
        0.0
    };
    (fx.clamp(0.0, 1.0), fy.clamp(0.0, 1.0))
}

/// Device points → integer device pixels (`point * scale`), clamped to the screen.
pub fn device_to_pixels(x: f64, y: f64, screen: &crate::ScreenSpec) -> (i32, i32) {
    let clamp = |v: f64, max_px: u32| -> i32 {
        let max = f64::from(max_px.saturating_sub(1));
        (v * screen.scale).round().clamp(0.0, max) as i32
    };
    (clamp(x, screen.width_px), clamp(y, screen.height_px))
}

/// Pixel of the oriented display (the frame's space) → pixel of the natural portrait panel.
/// The Android emulator's `sendTouch` takes panel coordinates even though its screenshots are
/// rotated upright. `panel` is the portrait spec; results are clamped to it.
pub fn panel_pixels(
    x: i32,
    y: i32,
    panel: &crate::ScreenSpec,
    orientation: crate::Orientation,
) -> (i32, i32) {
    use crate::Orientation::{LandscapeLeft, LandscapeRight, Portrait, PortraitUpsideDown};
    let spec = panel.oriented(false);
    let (w, h) = (spec.width_px as i32 - 1, spec.height_px as i32 - 1);
    let (px, py) = match orientation {
        Portrait => (x, y),
        LandscapeLeft => (w - y, x),
        PortraitUpsideDown => (w - x, h - y),
        LandscapeRight => (y, h - x),
    };
    (px.clamp(0, w), py.clamp(0, h))
}

#[cfg(test)]
mod tests;
