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
mod tests;
