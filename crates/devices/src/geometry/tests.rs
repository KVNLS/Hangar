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
    let (x, y) = view_to_device(106.0, 238.0, 213.333, 476.0, s.width_pt(), s.height_pt()).unwrap();
    assert_eq!(device_to_pixels(x, y, &s), (636, 1428));
}

#[test]
fn unit_is_clamped() {
    assert_eq!(device_to_unit(201.0, 437.0, 402.0, 874.0), (0.5, 0.5));
    assert_eq!(device_to_unit(-5.0, 2000.0, 402.0, 874.0), (0.0, 1.0));
}

#[test]
fn panel_mapping_per_orientation() {
    use crate::Orientation::*;
    let s = crate::ScreenSpec {
        width_px: 1280,
        height_px: 2856,
        scale: 3.0,
    };
    // Verified on the emulator: the Settings search bar at frame px (1430, 286) in
    // landscape-left is hit by panel touch (993, 1430).
    assert_eq!(panel_pixels(1430, 286, &s, LandscapeLeft), (993, 1430));
    assert_eq!(
        panel_pixels(1430, 286, &s, LandscapeRight),
        (286, 2855 - 1430)
    );
    assert_eq!(panel_pixels(100, 200, &s, Portrait), (100, 200));
    assert_eq!(panel_pixels(100, 200, &s, PortraitUpsideDown), (1179, 2655));
    // Frame corners land on panel corners; out-of-range input is clamped.
    assert_eq!(panel_pixels(0, 0, &s, LandscapeLeft), (1279, 0));
    assert_eq!(panel_pixels(2855, 1279, &s, LandscapeLeft), (0, 2855));
    assert_eq!(panel_pixels(5000, -5, &s, LandscapeRight), (0, 0));
}

#[test]
fn oriented_screen_spec_and_pixels() {
    let s = crate::ScreenSpec {
        width_px: 1280,
        height_px: 2856,
        scale: 3.0,
    };
    let l = s.oriented(true);
    assert_eq!((l.width_px, l.height_px), (2856, 1280));
    assert_eq!(l.oriented(true), l);
    assert_eq!(l.oriented(false), s);
    assert_eq!(
        device_to_pixels(l.width_pt(), l.height_pt(), &l),
        (2855, 1279)
    );
    // A frame-space click in a half-size letterboxed landscape view.
    let (x, y) = view_to_device(1000.0, 200.0, 2000.0, 896.0, l.width_pt(), l.height_pt()).unwrap();
    assert_eq!(device_to_pixels(x, y, &l), (1428, 286));
}
