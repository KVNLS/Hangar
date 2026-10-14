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

#[cfg(test)]
mod tests {
    use super::*;

    fn close(a: f64, b: f64) -> bool {
        (a - b).abs() < 1e-9
    }

    #[test]
    fn contain_letterboxes_horizontally() {
        // 402x874 pt phone in a 1000x874 view: full height, centred.
        let r = contain(1000.0, 874.0, 402.0, 874.0);
        assert!(close(r.width, 402.0));
        assert!(close(r.height, 874.0));
        assert!(close(r.x, 299.0));
        assert!(close(r.y, 0.0));
    }

    #[test]
    fn contain_letterboxes_vertically() {
        let r = contain(201.0, 1000.0, 402.0, 874.0);
        assert!(close(r.width, 201.0));
        assert!(close(r.height, 437.0));
        assert!(close(r.y, (1000.0 - 437.0) / 2.0));
    }

    #[test]
    fn maps_view_point_to_device_points() {
        // View is half the device size, no letterbox.
        let p = view_to_device(100.5, 218.5, 201.0, 437.0, 402.0, 874.0).unwrap();
        assert!(close(p.0, 201.0));
        assert!(close(p.1, 437.0));
        // Corners.
        let p = view_to_device(0.0, 0.0, 201.0, 437.0, 402.0, 874.0).unwrap();
        assert_eq!(p, (0.0, 0.0));
        let p = view_to_device(201.0, 437.0, 201.0, 437.0, 402.0, 874.0).unwrap();
        assert!(close(p.0, 402.0) && close(p.1, 874.0));
    }

    #[test]
    fn letterbox_clicks_are_ignored() {
        // Screen occupies x in [299, 701].
        assert!(view_to_device(100.0, 400.0, 1000.0, 874.0, 402.0, 874.0).is_none());
        let p = view_to_device(299.0 + 201.0, 437.0, 1000.0, 874.0, 402.0, 874.0).unwrap();
        assert!(close(p.0, 201.0) && close(p.1, 437.0));
    }

    #[test]
    fn degenerate_sizes() {
        assert!(view_to_device(1.0, 1.0, 0.0, 0.0, 402.0, 874.0).is_none());
        assert_eq!(device_to_unit(10.0, 10.0, 0.0, 0.0), (0.0, 0.0));
    }

    #[test]
    fn points_to_pixels() {
        let s = crate::ScreenSpec {
            width_px: 1280,
            height_px: 2856,
            scale: 3.0,
        };
        assert_eq!(device_to_pixels(100.0, 200.0, &s), (300, 600));
        assert_eq!(
            device_to_pixels(s.width_pt(), s.height_pt(), &s),
            (1279, 2855)
        );
        assert_eq!(device_to_pixels(-1.0, 0.4, &s), (0, 1));
        // A click mapped by view_to_device in a half-size view lands on the same pixel.
        let (x, y) =
            view_to_device(106.0, 238.0, 213.333, 476.0, s.width_pt(), s.height_pt()).unwrap();
        assert_eq!(device_to_pixels(x, y, &s), (636, 1428));
    }

    #[test]
    fn unit_is_clamped() {
        assert_eq!(device_to_unit(201.0, 437.0, 402.0, 874.0), (0.5, 0.5));
        assert_eq!(device_to_unit(-5.0, 2000.0, 402.0, 874.0), (0.0, 1.0));
    }
}
