//! Keyboard → byte encoding for the legacy (xterm-compatible) keyboard protocol.

/// A key press the terminal knows how to encode.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Key {
    /// A printable character, already shifted by the platform (e.g. `A`, `!`).
    Char(char),
    Enter,
    Backspace,
    Tab,
    Escape,
    Up,
    Down,
    Left,
    Right,
    Home,
    End,
    PageUp,
    PageDown,
    Insert,
    Delete,
    /// Function key F1..=F12.
    F(u8),
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Modifiers {
    pub shift: bool,
    pub alt: bool,
    pub ctrl: bool,
}

impl Modifiers {
    pub const NONE: Self = Self {
        shift: false,
        alt: false,
        ctrl: false,
    };

    fn any(self) -> bool {
        self.shift || self.alt || self.ctrl
    }

    /// xterm modifier parameter: 1 + shift + 2*alt + 4*ctrl.
    fn xterm_param(self) -> u8 {
        1 + u8::from(self.shift) + 2 * u8::from(self.alt) + 4 * u8::from(self.ctrl)
    }
}

/// Terminal modes that change key encoding.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct KeyMode {
    /// DECCKM: cursor keys send SS3 (`ESC O x`) instead of CSI.
    pub app_cursor: bool,
}

const ESC: u8 = 0x1b;

/// Encodes a key press into the bytes a program running in the PTY expects.
/// Returns `None` for combinations with no legacy encoding (e.g. F13+).
pub fn encode_key(key: Key, mods: Modifiers, mode: KeyMode) -> Option<Vec<u8>> {
    let mut out = Vec::with_capacity(8);
    match key {
        Key::Char(c) => encode_char(c, mods, &mut out),
        Key::Enter => alt_prefixed(mods, b'\r', &mut out),
        Key::Backspace => alt_prefixed(mods, if mods.ctrl { 0x08 } else { 0x7f }, &mut out),
        Key::Tab if mods.shift => out.extend_from_slice(b"\x1b[Z"),
        Key::Tab => alt_prefixed(mods, b'\t', &mut out),
        Key::Escape => alt_prefixed(mods, ESC, &mut out),
        Key::Up => cursor_key(b'A', mods, mode, &mut out),
        Key::Down => cursor_key(b'B', mods, mode, &mut out),
        Key::Right => cursor_key(b'C', mods, mode, &mut out),
        Key::Left => cursor_key(b'D', mods, mode, &mut out),
        Key::Home => cursor_key(b'H', mods, mode, &mut out),
        Key::End => cursor_key(b'F', mods, mode, &mut out),
        Key::Insert => tilde_key(2, mods, &mut out),
        Key::Delete => tilde_key(3, mods, &mut out),
        Key::PageUp => tilde_key(5, mods, &mut out),
        Key::PageDown => tilde_key(6, mods, &mut out),
        Key::F(n @ 1..=4) => {
            let letter = b'P' + (n - 1);
            if mods.any() {
                csi_with_mods(1, letter, mods, &mut out);
            } else {
                out.extend_from_slice(&[ESC, b'O', letter]);
            }
        }
        Key::F(n @ 5..=12) => {
            let code = [15, 17, 18, 19, 20, 21, 23, 24][usize::from(n - 5)];
            tilde_key(code, mods, &mut out);
        }
        Key::F(_) => return None,
    }
    Some(out)
}

/// Wraps pasted text in bracketed-paste markers when the program enabled them. Escapes and
/// other control characters (tab and line breaks aside) are dropped, as xterm does, so pasted
/// content can neither end the paste early nor reach the program as keystrokes.
pub fn encode_paste(text: &str, bracketed: bool) -> Vec<u8> {
    if bracketed {
        let clean: String = text
            .chars()
            .filter(|&c| !c.is_control() || matches!(c, '\t' | '\n' | '\r'))
            .collect();
        let mut out = Vec::with_capacity(clean.len() + 12);
        out.extend_from_slice(b"\x1b[200~");
        out.extend_from_slice(clean.as_bytes());
        out.extend_from_slice(b"\x1b[201~");
        out
    } else {
        text.replace("\r\n", "\r").replace('\n', "\r").into_bytes()
    }
}

fn alt_prefixed(mods: Modifiers, byte: u8, out: &mut Vec<u8>) {
    if mods.alt {
        out.push(ESC);
    }
    out.push(byte);
}

fn encode_char(c: char, mods: Modifiers, out: &mut Vec<u8>) {
    if mods.alt {
        out.push(ESC);
    }
    if mods.ctrl
        && let Some(byte) = ctrl_byte(c)
    {
        out.push(byte);
        return;
    }
    let mut buf = [0u8; 4];
    out.extend_from_slice(c.encode_utf8(&mut buf).as_bytes());
}

fn ctrl_byte(c: char) -> Option<u8> {
    let byte = match c {
        'a'..='z' => c as u8 - b'a' + 1,
        'A'..='Z' => c as u8 - b'A' + 1,
        '@' | ' ' | '2' => 0x00,
        '[' | '3' => 0x1b,
        '\\' | '4' => 0x1c,
        ']' | '5' => 0x1d,
        '^' | '6' => 0x1e,
        '_' | '-' | '7' | '/' => 0x1f,
        '?' | '8' => 0x7f,
        _ => return None,
    };
    Some(byte)
}

fn cursor_key(letter: u8, mods: Modifiers, mode: KeyMode, out: &mut Vec<u8>) {
    if mods.any() {
        csi_with_mods(1, letter, mods, out);
    } else if mode.app_cursor {
        out.extend_from_slice(&[ESC, b'O', letter]);
    } else {
        out.extend_from_slice(&[ESC, b'[', letter]);
    }
}

fn tilde_key(code: u8, mods: Modifiers, out: &mut Vec<u8>) {
    out.extend_from_slice(b"\x1b[");
    out.extend_from_slice(code.to_string().as_bytes());
    if mods.any() {
        out.push(b';');
        out.extend_from_slice(mods.xterm_param().to_string().as_bytes());
    }
    out.push(b'~');
}

fn csi_with_mods(code: u8, letter: u8, mods: Modifiers, out: &mut Vec<u8>) {
    out.extend_from_slice(b"\x1b[");
    out.extend_from_slice(code.to_string().as_bytes());
    out.push(b';');
    out.extend_from_slice(mods.xterm_param().to_string().as_bytes());
    out.push(letter);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn enc(key: Key, mods: Modifiers) -> Vec<u8> {
        encode_key(key, mods, KeyMode::default()).unwrap()
    }

    const CTRL: Modifiers = Modifiers {
        shift: false,
        alt: false,
        ctrl: true,
    };
    const ALT: Modifiers = Modifiers {
        shift: false,
        alt: true,
        ctrl: false,
    };
    const SHIFT: Modifiers = Modifiers {
        shift: true,
        alt: false,
        ctrl: false,
    };

    #[test]
    fn plain_characters_are_utf8() {
        assert_eq!(enc(Key::Char('a'), Modifiers::NONE), b"a");
        assert_eq!(enc(Key::Char('é'), Modifiers::NONE), "é".as_bytes());
    }

    #[test]
    fn control_combinations() {
        assert_eq!(enc(Key::Char('c'), CTRL), [0x03]);
        assert_eq!(enc(Key::Char('C'), CTRL), [0x03]);
        assert_eq!(enc(Key::Char('a'), CTRL), [0x01]);
        assert_eq!(enc(Key::Char('z'), CTRL), [0x1a]);
        assert_eq!(enc(Key::Char(' '), CTRL), [0x00]);
        assert_eq!(enc(Key::Char('['), CTRL), [0x1b]);
        assert_eq!(enc(Key::Char('\\'), CTRL), [0x1c]);
        assert_eq!(enc(Key::Char('_'), CTRL), [0x1f]);
    }

    #[test]
    fn alt_prefixes_escape() {
        assert_eq!(enc(Key::Char('b'), ALT), b"\x1bb");
        assert_eq!(
            enc(
                Key::Char('x'),
                Modifiers {
                    alt: true,
                    ctrl: true,
                    shift: false
                }
            ),
            [0x1b, 0x18]
        );
        assert_eq!(enc(Key::Enter, ALT), b"\x1b\r");
        assert_eq!(enc(Key::Backspace, ALT), b"\x1b\x7f");
    }

    #[test]
    fn editing_keys() {
        assert_eq!(enc(Key::Enter, Modifiers::NONE), b"\r");
        assert_eq!(enc(Key::Backspace, Modifiers::NONE), [0x7f]);
        assert_eq!(enc(Key::Backspace, CTRL), [0x08]);
        assert_eq!(enc(Key::Tab, Modifiers::NONE), b"\t");
        assert_eq!(enc(Key::Tab, SHIFT), b"\x1b[Z");
        assert_eq!(enc(Key::Escape, Modifiers::NONE), b"\x1b");
        assert_eq!(enc(Key::Delete, Modifiers::NONE), b"\x1b[3~");
        assert_eq!(enc(Key::Insert, Modifiers::NONE), b"\x1b[2~");
        assert_eq!(enc(Key::PageUp, Modifiers::NONE), b"\x1b[5~");
        assert_eq!(enc(Key::PageDown, SHIFT), b"\x1b[6;2~");
    }

    #[test]
    fn cursor_keys_honour_app_cursor_mode() {
        assert_eq!(enc(Key::Up, Modifiers::NONE), b"\x1b[A");
        assert_eq!(enc(Key::Left, Modifiers::NONE), b"\x1b[D");
        assert_eq!(enc(Key::Home, Modifiers::NONE), b"\x1b[H");
        assert_eq!(enc(Key::End, Modifiers::NONE), b"\x1b[F");
        let app = KeyMode { app_cursor: true };
        assert_eq!(
            encode_key(Key::Up, Modifiers::NONE, app).unwrap(),
            b"\x1bOA"
        );
        assert_eq!(
            encode_key(Key::End, Modifiers::NONE, app).unwrap(),
            b"\x1bOF"
        );
        // Modified cursor keys always use the CSI form.
        assert_eq!(encode_key(Key::Right, CTRL, app).unwrap(), b"\x1b[1;5C");
        assert_eq!(enc(Key::Left, ALT), b"\x1b[1;3D");
        assert_eq!(enc(Key::Up, SHIFT), b"\x1b[1;2A");
    }

    #[test]
    fn function_keys() {
        assert_eq!(enc(Key::F(1), Modifiers::NONE), b"\x1bOP");
        assert_eq!(enc(Key::F(4), Modifiers::NONE), b"\x1bOS");
        assert_eq!(enc(Key::F(1), SHIFT), b"\x1b[1;2P");
        assert_eq!(enc(Key::F(5), Modifiers::NONE), b"\x1b[15~");
        assert_eq!(enc(Key::F(10), Modifiers::NONE), b"\x1b[21~");
        assert_eq!(enc(Key::F(12), CTRL), b"\x1b[24;5~");
        assert_eq!(
            encode_key(Key::F(13), Modifiers::NONE, KeyMode::default()),
            None
        );
    }

    #[test]
    fn paste_encoding() {
        assert_eq!(encode_paste("a\nb", false), b"a\rb");
        assert_eq!(encode_paste("a\r\nb", false), b"a\rb");
        assert_eq!(encode_paste("hi", true), b"\x1b[200~hi\x1b[201~");
        assert_eq!(
            encode_paste("x\x1b[201~y", true),
            b"\x1b[200~x[201~y\x1b[201~"
        );
    }

    #[test]
    fn bracketed_paste_cannot_be_ended_early() {
        let pasted = encode_paste("a\x1b[20\x1b[201~1~rm -rf ~\r", true);
        assert_eq!(pasted, b"\x1b[200~a[20[201~1~rm -rf ~\r\x1b[201~");
        assert_eq!(
            encode_paste("tab\tnl\nbell\x07c1\u{9b}x", true),
            "\x1b[200~tab\tnl\nbellc1x\x1b[201~".as_bytes()
        );
    }
}
