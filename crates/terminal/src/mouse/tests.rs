use super::*;

const SHIFT: Modifiers = Modifiers {
    shift: true,
    alt: false,
    ctrl: false,
};
const ALT_CTRL: Modifiers = Modifiers {
    shift: false,
    alt: true,
    ctrl: true,
};

fn mode(tracking: MouseTracking, encoding: MouseEncoding) -> MouseMode {
    MouseMode { tracking, encoding }
}

fn report(action: MouseAction, col: usize, row: usize, mods: Modifiers) -> MouseReport {
    MouseReport {
        action,
        col,
        row,
        mods,
    }
}

fn sgr(action: MouseAction, col: usize, row: usize, mods: Modifiers) -> Option<String> {
    let mode = mode(MouseTracking::Motion, MouseEncoding::Sgr);
    encode_mouse(report(action, col, row, mods), mode).map(|b| String::from_utf8(b).unwrap())
}

#[test]
fn sgr_press_release_per_button() {
    use MouseAction::{Press, Release};
    let none = Modifiers::NONE;
    assert_eq!(
        sgr(Press(MouseButton::Left), 0, 0, none).as_deref(),
        Some("\x1b[<0;1;1M")
    );
    assert_eq!(
        sgr(Press(MouseButton::Middle), 4, 2, none).as_deref(),
        Some("\x1b[<1;5;3M")
    );
    assert_eq!(
        sgr(Press(MouseButton::Right), 9, 0, none).as_deref(),
        Some("\x1b[<2;10;1M")
    );
    // SGR releases keep the button and use a lowercase final byte.
    assert_eq!(
        sgr(Release(MouseButton::Right), 9, 0, none).as_deref(),
        Some("\x1b[<2;10;1m")
    );
}

#[test]
fn sgr_modifiers_wheel_and_large_coordinates() {
    use MouseAction::{Press, Release};
    assert_eq!(
        sgr(Press(MouseButton::Left), 0, 0, SHIFT).as_deref(),
        Some("\x1b[<4;1;1M")
    );
    assert_eq!(
        sgr(Press(MouseButton::Left), 0, 0, ALT_CTRL).as_deref(),
        Some("\x1b[<24;1;1M")
    );
    assert_eq!(
        sgr(Press(MouseButton::WheelUp), 2, 3, Modifiers::NONE).as_deref(),
        Some("\x1b[<64;3;4M")
    );
    assert_eq!(
        sgr(Press(MouseButton::WheelDown), 2, 3, ALT_CTRL).as_deref(),
        Some("\x1b[<89;3;4M")
    );
    assert_eq!(
        sgr(Press(MouseButton::WheelLeft), 0, 0, Modifiers::NONE).as_deref(),
        Some("\x1b[<66;1;1M")
    );
    assert_eq!(
        sgr(Release(MouseButton::WheelUp), 0, 0, Modifiers::NONE),
        None
    );
    assert_eq!(
        sgr(Press(MouseButton::Left), 499, 299, Modifiers::NONE).as_deref(),
        Some("\x1b[<0;500;300M")
    );
}

#[test]
fn motion_depends_on_tracking_mode() {
    let drag = MouseAction::Move(Some(MouseButton::Left));
    let hover = MouseAction::Move(None);
    let at = |action, tracking| {
        encode_mouse(
            report(action, 1, 1, Modifiers::NONE),
            mode(tracking, MouseEncoding::Sgr),
        )
        .map(|b| String::from_utf8(b).unwrap())
    };
    assert_eq!(at(drag, MouseTracking::Click), None);
    assert_eq!(at(hover, MouseTracking::Click), None);
    assert_eq!(
        at(drag, MouseTracking::Drag).as_deref(),
        Some("\x1b[<32;2;2M")
    );
    assert_eq!(at(hover, MouseTracking::Drag), None);
    assert_eq!(
        at(drag, MouseTracking::Motion).as_deref(),
        Some("\x1b[<32;2;2M")
    );
    assert_eq!(
        at(hover, MouseTracking::Motion).as_deref(),
        Some("\x1b[<35;2;2M")
    );
    let right_drag = MouseAction::Move(Some(MouseButton::Right));
    assert_eq!(
        at(right_drag, MouseTracking::Drag).as_deref(),
        Some("\x1b[<34;2;2M")
    );
}

#[test]
fn nothing_is_reported_when_tracking_is_off() {
    let off = mode(MouseTracking::Off, MouseEncoding::Sgr);
    let press = report(MouseAction::Press(MouseButton::Left), 0, 0, Modifiers::NONE);
    assert_eq!(encode_mouse(press, off), None);
    assert!(!off.is_active());
}

#[test]
fn x10_press_release_and_modifiers() {
    let x10 = mode(MouseTracking::Click, MouseEncoding::X10);
    let press = report(MouseAction::Press(MouseButton::Left), 0, 0, Modifiers::NONE);
    assert_eq!(encode_mouse(press, x10).unwrap(), b"\x1b[M\x20\x21\x21");
    let release = report(
        MouseAction::Release(MouseButton::Middle),
        9,
        4,
        Modifiers::NONE,
    );
    // Legacy release is button 3 whichever button went up.
    assert_eq!(encode_mouse(release, x10).unwrap(), b"\x1b[M\x23\x2a\x25");
    let right_ctrl = report(MouseAction::Press(MouseButton::Right), 0, 0, ALT_CTRL);
    assert_eq!(
        encode_mouse(right_ctrl, x10).unwrap(),
        [0x1b, b'[', b'M', 32 + 2 + 24, 33, 33]
    );
    let wheel = report(
        MouseAction::Press(MouseButton::WheelDown),
        0,
        0,
        Modifiers::NONE,
    );
    assert_eq!(
        encode_mouse(wheel, x10).unwrap(),
        [0x1b, b'[', b'M', 32 + 65, 33, 33]
    );
    let wheel_release = report(
        MouseAction::Release(MouseButton::WheelDown),
        0,
        0,
        Modifiers::NONE,
    );
    assert_eq!(encode_mouse(wheel_release, x10), None);
}

#[test]
fn x10_drag_and_coordinate_limit() {
    let x10 = mode(MouseTracking::Drag, MouseEncoding::X10);
    let drag = report(MouseAction::Move(Some(MouseButton::Left)), 2, 3, SHIFT);
    assert_eq!(
        encode_mouse(drag, x10).unwrap(),
        [0x1b, b'[', b'M', 32 + 32 + 4, 35, 36]
    );
    let edge = report(
        MouseAction::Press(MouseButton::Left),
        222,
        0,
        Modifiers::NONE,
    );
    assert_eq!(encode_mouse(edge, x10).unwrap()[4], 255);
    let beyond = report(
        MouseAction::Press(MouseButton::Left),
        223,
        0,
        Modifiers::NONE,
    );
    assert_eq!(encode_mouse(beyond, x10), None);
}

#[test]
fn utf8_encoding_extends_coordinates() {
    let utf8 = mode(MouseTracking::Click, MouseEncoding::Utf8);
    let small = report(MouseAction::Press(MouseButton::Left), 0, 0, Modifiers::NONE);
    assert_eq!(encode_mouse(small, utf8).unwrap(), b"\x1b[M\x20\x21\x21");
    let wide = report(
        MouseAction::Press(MouseButton::Left),
        299,
        0,
        Modifiers::NONE,
    );
    // 299 + 1 + 32 = 332 = U+014C, two UTF-8 bytes.
    assert_eq!(encode_mouse(wide, utf8).unwrap(), b"\x1b[M\x20\xc5\x8c\x21");
    let beyond = report(
        MouseAction::Press(MouseButton::Left),
        2015,
        0,
        Modifiers::NONE,
    );
    assert_eq!(encode_mouse(beyond, utf8), None);
}

#[test]
fn focus_sequences() {
    assert_eq!(encode_focus(true), b"\x1b[I");
    assert_eq!(encode_focus(false), b"\x1b[O");
}
