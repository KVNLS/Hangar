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
fn landscape_frames_swap_the_screen_sides() {
    let spec = hangar_devices::ScreenSpec {
        width_px: 1280,
        height_px: 2856,
        scale: 3.0,
    };
    let screen = screen_points(Some(spec), Some((2856, 1280))).unwrap();
    assert!((screen.0 - 952.0).abs() < 1e-9 && (screen.1 - 1280.0 / 3.0).abs() < 1e-9);
    // A 1428x640 view (half the frame) at scale 1: a click in its centre is the screen centre.
    let image = image_rect(bounds(0.0, 0.0, 1428.0, 640.0), 2856.0, 1280.0, 1.0);
    let p = to_device(714.0, 320.0, image, screen).unwrap();
    assert!((p.0 - 476.0).abs() < 1e-9 && (p.1 - 640.0 / 3.0).abs() < 1e-9);
    // A tall view letterboxes the landscape frame vertically; the bars are not clickable.
    let image = image_rect(bounds(0.0, 0.0, 714.0, 1000.0), 2856.0, 1280.0, 1.0);
    assert!(to_device(100.0, 10.0, image, screen).is_none());
    assert!(to_device(100.0, 500.0, image, screen).is_some());
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
