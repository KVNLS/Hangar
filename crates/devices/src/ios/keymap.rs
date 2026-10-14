//! US-layout character → USB HID keyboard usage (page 0x07) mapping used for Indigo keyboard
//! events. Pure, so it is testable anywhere.

/// HID usage of Left Shift.
pub(crate) const USAGE_LEFT_SHIFT: u32 = 0xe1;
pub(crate) const USAGE_RETURN: u32 = 0x28;
pub(crate) const USAGE_TAB: u32 = 0x2b;

/// HID keyboard usage of a non-text key.
pub(crate) fn usage_for_key(key: crate::Key) -> u32 {
    use crate::Key;
    match key {
        Key::Backspace => 0x2a,
        Key::Enter => USAGE_RETURN,
        Key::Tab => USAGE_TAB,
        Key::Escape => 0x29,
        Key::ArrowRight => 0x4f,
        Key::ArrowLeft => 0x50,
        Key::ArrowDown => 0x51,
        Key::ArrowUp => 0x52,
    }
}

/// Returns `(usage, needs_shift)` for a character on a US keyboard, or `None` if the character
/// has no key (non-ASCII, control characters other than newline and tab).
pub(crate) fn usage_for(c: char) -> Option<(u32, bool)> {
    let u = |usage: u32| Some((usage, false));
    let s = |usage: u32| Some((usage, true));
    match c {
        'a'..='z' => u(0x04 + (c as u32 - 'a' as u32)),
        'A'..='Z' => s(0x04 + (c as u32 - 'A' as u32)),
        '1'..='9' => u(0x1e + (c as u32 - '1' as u32)),
        '0' => u(0x27),
        '\n' | '\r' => u(USAGE_RETURN),
        '\t' => u(USAGE_TAB),
        ' ' => u(0x2c),
        '-' => u(0x2d),
        '_' => s(0x2d),
        '=' => u(0x2e),
        '+' => s(0x2e),
        '[' => u(0x2f),
        '{' => s(0x2f),
        ']' => u(0x30),
        '}' => s(0x30),
        '\\' => u(0x31),
        '|' => s(0x31),
        ';' => u(0x33),
        ':' => s(0x33),
        '\'' => u(0x34),
        '"' => s(0x34),
        '`' => u(0x35),
        '~' => s(0x35),
        ',' => u(0x36),
        '<' => s(0x36),
        '.' => u(0x37),
        '>' => s(0x37),
        '/' => u(0x38),
        '?' => s(0x38),
        '!' => s(0x1e),
        '@' => s(0x1f),
        '#' => s(0x20),
        '$' => s(0x21),
        '%' => s(0x22),
        '^' => s(0x23),
        '&' => s(0x24),
        '*' => s(0x25),
        '(' => s(0x26),
        ')' => s(0x27),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn letters_digits_and_shift() {
        assert_eq!(usage_for('a'), Some((0x04, false)));
        assert_eq!(usage_for('Z'), Some((0x1d, true)));
        assert_eq!(usage_for('1'), Some((0x1e, false)));
        assert_eq!(usage_for('0'), Some((0x27, false)));
        assert_eq!(usage_for('!'), Some((0x1e, true)));
        assert_eq!(usage_for('?'), Some((0x38, true)));
        assert_eq!(usage_for('\n'), Some((USAGE_RETURN, false)));
    }

    #[test]
    fn named_keys() {
        use crate::Key;
        assert_eq!(usage_for_key(Key::Backspace), 0x2a);
        assert_eq!(usage_for_key(Key::Escape), 0x29);
        assert_eq!(usage_for_key(Key::ArrowUp), 0x52);
        assert_eq!(usage_for_key(Key::Enter), USAGE_RETURN);
    }

    #[test]
    fn unmapped() {
        assert_eq!(usage_for('é'), None);
        assert_eq!(usage_for('\u{7}'), None);
    }

    #[test]
    fn printable_ascii_all_mapped() {
        for c in ' '..='~' {
            assert!(usage_for(c).is_some(), "{c:?}");
        }
    }
}
