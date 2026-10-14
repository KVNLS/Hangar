//! Mouse and focus reporting: encodes pointer events for programs that enabled xterm mouse
//! tracking (DECSET 1000/1002/1003, encodings X10 / UTF-8 1005 / SGR 1006) and focus events
//! (DECSET 1004).

use crate::keys::Modifiers;

/// Which pointer events the program asked for.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord)]
pub enum MouseTracking {
    /// No reporting: the pointer belongs to the user (selection, scrollback).
    #[default]
    Off,
    /// 1000: presses, releases and wheel.
    Click,
    /// 1002: plus motion while a button is held.
    Drag,
    /// 1003: plus all motion.
    Motion,
}

/// How reports are encoded.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum MouseEncoding {
    /// `CSI M Cb Cx Cy` with single-byte values (coordinates up to 223).
    #[default]
    X10,
    /// 1005: like X10 but values above 127 are UTF-8 encoded (coordinates up to 2015).
    Utf8,
    /// 1006: `CSI < b ; x ; y M|m`, unbounded coordinates, release keeps the button.
    Sgr,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct MouseMode {
    pub tracking: MouseTracking,
    pub encoding: MouseEncoding,
}

impl MouseMode {
    pub fn is_active(self) -> bool {
        self.tracking != MouseTracking::Off
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MouseButton {
    Left,
    Middle,
    Right,
    WheelUp,
    WheelDown,
    WheelLeft,
    WheelRight,
}

impl MouseButton {
    fn code(self) -> u8 {
        match self {
            Self::Left => 0,
            Self::Middle => 1,
            Self::Right => 2,
            Self::WheelUp => 64,
            Self::WheelDown => 65,
            Self::WheelLeft => 66,
            Self::WheelRight => 67,
        }
    }

    fn is_wheel(self) -> bool {
        self.code() >= 64
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MouseAction {
    Press(MouseButton),
    Release(MouseButton),
    /// Pointer moved to a new cell; `Some` while a button is held (a drag).
    Move(Option<MouseButton>),
}

/// A pointer event on the grid. `col`/`row` are 0-based viewport cells.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MouseReport {
    pub action: MouseAction,
    pub col: usize,
    pub row: usize,
    pub mods: Modifiers,
}

const MOTION: u8 = 32;
/// Legacy encodings cannot say which button was released.
const LEGACY_RELEASE: u8 = 3;

/// Encodes a pointer event for the program, or `None` when the current mode does not report
/// it (or, for the legacy encodings, the position is out of range).
pub fn encode_mouse(report: MouseReport, mode: MouseMode) -> Option<Vec<u8>> {
    let (mut code, release) = match report.action {
        MouseAction::Press(button) => (button.code(), false),
        // Wheel "buttons" have no release.
        MouseAction::Release(button) if button.is_wheel() => return None,
        MouseAction::Release(button) => match mode.encoding {
            MouseEncoding::Sgr => (button.code(), true),
            MouseEncoding::X10 | MouseEncoding::Utf8 => (LEGACY_RELEASE, true),
        },
        MouseAction::Move(button) => {
            let reported = match (mode.tracking, button) {
                (MouseTracking::Motion, _) | (MouseTracking::Drag, Some(_)) => true,
                (MouseTracking::Drag, None) | (MouseTracking::Click | MouseTracking::Off, _) => {
                    false
                }
            };
            if !reported {
                return None;
            }
            (
                button.map_or(LEGACY_RELEASE, MouseButton::code) + MOTION,
                false,
            )
        }
    };
    if !mode.is_active() {
        return None;
    }
    code += 4 * u8::from(report.mods.shift)
        + 8 * u8::from(report.mods.alt)
        + 16 * u8::from(report.mods.ctrl);

    match mode.encoding {
        MouseEncoding::Sgr => Some(
            format!(
                "\x1b[<{code};{};{}{}",
                report.col + 1,
                report.row + 1,
                if release { 'm' } else { 'M' }
            )
            .into_bytes(),
        ),
        MouseEncoding::X10 => {
            let mut out = b"\x1b[M".to_vec();
            for value in [usize::from(code), report.col + 1, report.row + 1] {
                out.push(u8::try_from(value + 32).ok()?);
            }
            Some(out)
        }
        MouseEncoding::Utf8 => {
            let mut out = b"\x1b[M".to_vec();
            for value in [usize::from(code), report.col + 1, report.row + 1] {
                let value = u32::try_from(value + 32).ok()?;
                // Two-byte UTF-8 sequences are the limit of the 1005 protocol.
                if value > 0x7ff {
                    return None;
                }
                let c = char::from_u32(value)?;
                let mut buf = [0; 4];
                out.extend_from_slice(c.encode_utf8(&mut buf).as_bytes());
            }
            Some(out)
        }
    }
}

/// Focus event for DECSET 1004: `CSI I` on focus in, `CSI O` on focus out.
pub fn encode_focus(focused: bool) -> &'static [u8] {
    if focused { b"\x1b[I" } else { b"\x1b[O" }
}

#[cfg(test)]
mod tests {
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
}
