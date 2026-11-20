//! USB HID usage (keyboard page 0x07) <-> Linux evdev key codes.
//!
//! Hopwyn sends physical key positions, not characters, so each receiver
//! applies its own keyboard layout. The table follows the Linux kernel's
//! HID keyboard mapping (drivers/hid/hid-input.c).

use hopwyn_proto::Button;

/// (HID usage, evdev code) pairs. Each evdev code appears once, so the
/// mapping is reversible.
const TABLE: &[(u16, u16)] = &[
    // Letters a..z
    (0x04, 30),
    (0x05, 48),
    (0x06, 46),
    (0x07, 32),
    (0x08, 18),
    (0x09, 33),
    (0x0a, 34),
    (0x0b, 35),
    (0x0c, 23),
    (0x0d, 36),
    (0x0e, 37),
    (0x0f, 38),
    (0x10, 50),
    (0x11, 49),
    (0x12, 24),
    (0x13, 25),
    (0x14, 16),
    (0x15, 19),
    (0x16, 31),
    (0x17, 20),
    (0x18, 22),
    (0x19, 47),
    (0x1a, 17),
    (0x1b, 45),
    (0x1c, 21),
    (0x1d, 44),
    // Digits 1..9, 0
    (0x1e, 2),
    (0x1f, 3),
    (0x20, 4),
    (0x21, 5),
    (0x22, 6),
    (0x23, 7),
    (0x24, 8),
    (0x25, 9),
    (0x26, 10),
    (0x27, 11),
    // Enter, Esc, Backspace, Tab, Space, - = [ ] \
    (0x28, 28),
    (0x29, 1),
    (0x2a, 14),
    (0x2b, 15),
    (0x2c, 57),
    (0x2d, 12),
    (0x2e, 13),
    (0x2f, 26),
    (0x30, 27),
    (0x31, 43),
    // ; ' ` , . / CapsLock
    (0x33, 39),
    (0x34, 40),
    (0x35, 41),
    (0x36, 51),
    (0x37, 52),
    (0x38, 53),
    (0x39, 58),
    // F1..F12
    (0x3a, 59),
    (0x3b, 60),
    (0x3c, 61),
    (0x3d, 62),
    (0x3e, 63),
    (0x3f, 64),
    (0x40, 65),
    (0x41, 66),
    (0x42, 67),
    (0x43, 68),
    (0x44, 87),
    (0x45, 88),
    // PrintScreen, ScrollLock, Pause, Insert, Home, PageUp, Delete, End, PageDown
    (0x46, 99),
    (0x47, 70),
    (0x48, 119),
    (0x49, 110),
    (0x4a, 102),
    (0x4b, 104),
    (0x4c, 111),
    (0x4d, 107),
    (0x4e, 109),
    // Right, Left, Down, Up
    (0x4f, 106),
    (0x50, 105),
    (0x51, 108),
    (0x52, 103),
    // Keypad: NumLock / * - + Enter 1..9 0 .
    (0x53, 69),
    (0x54, 98),
    (0x55, 55),
    (0x56, 74),
    (0x57, 78),
    (0x58, 96),
    (0x59, 79),
    (0x5a, 80),
    (0x5b, 81),
    (0x5c, 75),
    (0x5d, 76),
    (0x5e, 77),
    (0x5f, 71),
    (0x60, 72),
    (0x61, 73),
    (0x62, 82),
    (0x63, 83),
    // Non-US \, Menu, Power, Keypad =
    (0x64, 86),
    (0x65, 127),
    (0x66, 116),
    (0x67, 117),
    // F13..F24
    (0x68, 183),
    (0x69, 184),
    (0x6a, 185),
    (0x6b, 186),
    (0x6c, 187),
    (0x6d, 188),
    (0x6e, 189),
    (0x6f, 190),
    (0x70, 191),
    (0x71, 192),
    (0x72, 193),
    (0x73, 194),
    // Mute, Volume up, Volume down
    (0x7f, 113),
    (0x80, 115),
    (0x81, 114),
    // International: Ro, Katakana/Hiragana, Yen, Henkan, Muhenkan, Hangeul, Hanja
    (0x87, 89),
    (0x88, 93),
    (0x89, 124),
    (0x8a, 92),
    (0x8b, 94),
    (0x90, 122),
    (0x91, 123),
    // Left Ctrl, Shift, Alt, Meta; Right Ctrl, Shift, Alt, Meta
    (0xe0, 29),
    (0xe1, 42),
    (0xe2, 56),
    (0xe3, 125),
    (0xe4, 97),
    (0xe5, 54),
    (0xe6, 100),
    (0xe7, 126),
];

/// HID usages the kernel maps onto a key already in [`TABLE`]: accepted
/// when received, never produced when capturing.
const ALIASES: &[(u16, u16)] = &[
    (0x32, 43), // Non-US # sits where \ is on ISO keyboards
];

/// The evdev code for a HID usage, or `None` for keys Hopwyn doesn't map.
pub fn hid_to_evdev(hid: u16) -> Option<u16> {
    TABLE
        .iter()
        .chain(ALIASES)
        .find(|(h, _)| *h == hid)
        .map(|(_, e)| *e)
}

/// The HID usage for an evdev code, or `None` if it has no mapping.
pub fn evdev_to_hid(code: u16) -> Option<u16> {
    TABLE.iter().find(|(_, e)| *e == code).map(|(h, _)| *h)
}

/// Linux evdev code for a mouse button (BTN_LEFT and friends).
pub fn button_to_evdev(button: Button) -> u16 {
    match button {
        Button::Left => 0x110,
        Button::Right => 0x111,
        Button::Middle => 0x112,
        Button::Back => 0x113,
        Button::Forward => 0x114,
    }
}

/// The button for a Linux evdev code.
pub fn evdev_to_button(code: u16) -> Option<Button> {
    match code {
        0x110 => Some(Button::Left),
        0x111 => Some(Button::Right),
        0x112 => Some(Button::Middle),
        0x113 => Some(Button::Back),
        0x114 => Some(Button::Forward),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use super::*;

    #[test]
    fn table_has_no_duplicates() {
        let hids: HashSet<u16> = TABLE.iter().map(|(h, _)| *h).collect();
        let codes: HashSet<u16> = TABLE.iter().map(|(_, e)| *e).collect();
        assert_eq!(hids.len(), TABLE.len(), "duplicate HID usage");
        assert_eq!(codes.len(), TABLE.len(), "duplicate evdev code");
    }

    #[test]
    fn every_entry_round_trips() {
        for &(hid, code) in TABLE {
            assert_eq!(hid_to_evdev(hid), Some(code));
            assert_eq!(evdev_to_hid(code), Some(hid));
        }
    }

    #[test]
    fn known_keys() {
        assert_eq!(hid_to_evdev(0x04), Some(30)); // A
        assert_eq!(hid_to_evdev(0x1d), Some(44)); // Z
        assert_eq!(hid_to_evdev(0x27), Some(11)); // 0
        assert_eq!(hid_to_evdev(0x2c), Some(57)); // Space
        assert_eq!(hid_to_evdev(0xe3), Some(125)); // Left Meta
    }

    #[test]
    fn aliases_map_in_but_not_out() {
        assert_eq!(hid_to_evdev(0x32), Some(43));
        assert_eq!(evdev_to_hid(43), Some(0x31));
    }

    #[test]
    fn unmapped_keys_are_dropped() {
        assert_eq!(hid_to_evdev(0x00), None);
        assert_eq!(hid_to_evdev(0x03), None);
        assert_eq!(hid_to_evdev(0xffff), None);
        assert_eq!(evdev_to_hid(0), None);
    }

    #[test]
    fn buttons_round_trip() {
        for b in [
            Button::Left,
            Button::Right,
            Button::Middle,
            Button::Back,
            Button::Forward,
        ] {
            assert_eq!(evdev_to_button(button_to_evdev(b)), Some(b));
        }
    }
}
