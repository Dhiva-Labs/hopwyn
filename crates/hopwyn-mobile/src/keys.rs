//! Typed text to USB HID key usages (page 0x07), as a US keyboard types
//! it. The computer maps usages through its own layout, exactly as it does
//! for a real keyboard plugged into another Hopwyn computer.

/// Left Shift.
pub const SHIFT: u16 = 0xe1;
pub const ENTER: u16 = 0x28;
pub const BACKSPACE: u16 = 0x2a;
pub const TAB: u16 = 0x2b;
pub const ESCAPE: u16 = 0x29;
pub const RIGHT: u16 = 0x4f;
pub const LEFT: u16 = 0x50;
pub const DOWN: u16 = 0x51;
pub const UP: u16 = 0x52;

/// The key for `c` and whether Shift is held, or `None` if a US keyboard
/// has no key for it (such characters are skipped).
pub fn for_char(c: char) -> Option<(u16, bool)> {
    let lower = |c: char| u16::try_from(u32::from(c) - u32::from('a')).ok();
    Some(match c {
        'a'..='z' => (0x04 + lower(c)?, false),
        'A'..='Z' => (0x04 + lower(c.to_ascii_lowercase())?, true),
        '1'..='9' => (
            0x1e + u16::try_from(u32::from(c) - u32::from('1')).ok()?,
            false,
        ),
        '0' => (0x27, false),
        '\n' => (ENTER, false),
        '\t' => (TAB, false),
        ' ' => (0x2c, false),
        '-' => (0x2d, false),
        '=' => (0x2e, false),
        '[' => (0x2f, false),
        ']' => (0x30, false),
        '\\' => (0x31, false),
        ';' => (0x33, false),
        '\'' => (0x34, false),
        '`' => (0x35, false),
        ',' => (0x36, false),
        '.' => (0x37, false),
        '/' => (0x38, false),
        '!' => (0x1e, true),
        '@' => (0x1f, true),
        '#' => (0x20, true),
        '$' => (0x21, true),
        '%' => (0x22, true),
        '^' => (0x23, true),
        '&' => (0x24, true),
        '*' => (0x25, true),
        '(' => (0x26, true),
        ')' => (0x27, true),
        '_' => (0x2d, true),
        '+' => (0x2e, true),
        '{' => (0x2f, true),
        '}' => (0x30, true),
        '|' => (0x31, true),
        ':' => (0x33, true),
        '"' => (0x34, true),
        '~' => (0x35, true),
        '<' => (0x36, true),
        '>' => (0x37, true),
        '?' => (0x38, true),
        _ => return None,
    })
}

/// Shift keys (left, right): held while typing capitals and symbols.
pub fn is_shift(code: u16) -> bool {
    code == SHIFT || code == 0xe5
}

/// The Windows/Super keys (left, right).
pub fn is_super(code: u16) -> bool {
    code == 0xe3 || code == 0xe7
}

/// The character a key types on a US keyboard, the reverse of [`for_char`]
/// (letters, digits, symbols, space). `None` for keys that type nothing.
pub fn to_char(code: u16, shift: bool) -> Option<char> {
    if (0x04..=0x1d).contains(&code) {
        let c = char::from(b'a' + u8::try_from(code - 0x04).ok()?);
        return Some(if shift { c.to_ascii_uppercase() } else { c });
    }
    let (plain, shifted) = match code {
        0x1e => ('1', '!'),
        0x1f => ('2', '@'),
        0x20 => ('3', '#'),
        0x21 => ('4', '$'),
        0x22 => ('5', '%'),
        0x23 => ('6', '^'),
        0x24 => ('7', '&'),
        0x25 => ('8', '*'),
        0x26 => ('9', '('),
        0x27 => ('0', ')'),
        0x2c => (' ', ' '),
        0x2d => ('-', '_'),
        0x2e => ('=', '+'),
        0x2f => ('[', '{'),
        0x30 => (']', '}'),
        0x31 => ('\\', '|'),
        0x33 => (';', ':'),
        0x34 => ('\'', '"'),
        0x35 => ('`', '~'),
        0x36 => (',', '<'),
        0x37 => ('.', '>'),
        0x38 => ('/', '?'),
        _ => return None,
    };
    Some(if shift { shifted } else { plain })
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;

    #[test]
    fn letters_digits_and_symbols_map_like_a_us_keyboard() {
        assert_eq!(for_char('a'), Some((0x04, false)));
        assert_eq!(for_char('z'), Some((0x1d, false)));
        assert_eq!(for_char('Q'), Some((0x14, true)));
        assert_eq!(for_char('1'), Some((0x1e, false)));
        assert_eq!(for_char('9'), Some((0x26, false)));
        assert_eq!(for_char('0'), Some((0x27, false)));
        assert_eq!(for_char('@'), Some((0x1f, true)));
        assert_eq!(for_char('?'), Some((0x38, true)));
        assert_eq!(for_char(' '), Some((0x2c, false)));
        assert_eq!(for_char('\n'), Some((ENTER, false)));
        assert_eq!(for_char('é'), None);
    }

    #[test]
    fn keys_type_back_the_characters_they_came_from() {
        for c in "Hello, World! 123 ~_+{}|:\"<>? -=[]\\;'`,./".chars() {
            let (code, shift) = for_char(c).unwrap();
            assert_eq!(to_char(code, shift), Some(c), "{c:?}");
        }
        assert_eq!(to_char(ENTER, false), None);
        assert!(is_shift(0xe5) && is_super(0xe3));
    }
}
