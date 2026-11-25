//! Mapping from the HID usages of `device::human_interface` to Linux input event codes
//! (`linux/input-event-codes.h`). Usages without a mapping are skipped by the controller.

use device::human_interface::{consumer, keyboard, system};

// Linux event types and codes (linux/input-event-codes.h).
pub const EV_SYN: u16 = 0x00;
pub const EV_KEY: u16 = 0x01;
pub const EV_REL: u16 = 0x02;
pub const EV_LED: u16 = 0x11;
pub const SYN_REPORT: u16 = 0;

pub const REL_X: u16 = 0x00;
pub const REL_Y: u16 = 0x01;
pub const REL_HWHEEL: u16 = 0x06;
pub const REL_WHEEL: u16 = 0x08;

pub const LED_NUML: u16 = 0x00;
pub const LED_CAPSL: u16 = 0x01;
pub const LED_SCROLLL: u16 = 0x02;

pub const BTN_LEFT: u16 = 0x110;
pub const BTN_RIGHT: u16 = 0x111;
pub const BTN_MIDDLE: u16 = 0x112;

pub const KEY_ESC: u16 = 1;
pub const KEY_1: u16 = 2;
pub const KEY_0: u16 = 11;
pub const KEY_MINUS: u16 = 12;
pub const KEY_EQUAL: u16 = 13;
pub const KEY_BACKSPACE: u16 = 14;
pub const KEY_TAB: u16 = 15;
pub const KEY_LEFTBRACE: u16 = 26;
pub const KEY_RIGHTBRACE: u16 = 27;
pub const KEY_ENTER: u16 = 28;
pub const KEY_LEFTCTRL: u16 = 29;
pub const KEY_A: u16 = 30;
pub const KEY_SEMICOLON: u16 = 39;
pub const KEY_APOSTROPHE: u16 = 40;
pub const KEY_GRAVE: u16 = 41;
pub const KEY_LEFTSHIFT: u16 = 42;
pub const KEY_BACKSLASH: u16 = 43;
pub const KEY_RIGHTSHIFT: u16 = 54;
pub const KEY_KPASTERISK: u16 = 55;
pub const KEY_LEFTALT: u16 = 56;
pub const KEY_SPACE: u16 = 57;
pub const KEY_CAPSLOCK: u16 = 58;
pub const KEY_F1: u16 = 59;
pub const KEY_NUMLOCK: u16 = 69;
pub const KEY_SCROLLLOCK: u16 = 70;
pub const KEY_KP7: u16 = 71;
pub const KEY_KP8: u16 = 72;
pub const KEY_KP9: u16 = 73;
pub const KEY_KPMINUS: u16 = 74;
pub const KEY_KP4: u16 = 75;
pub const KEY_KP5: u16 = 76;
pub const KEY_KP6: u16 = 77;
pub const KEY_KPPLUS: u16 = 78;
pub const KEY_KP1: u16 = 79;
pub const KEY_KP2: u16 = 80;
pub const KEY_KP3: u16 = 81;
pub const KEY_KP0: u16 = 82;
pub const KEY_KPDOT: u16 = 83;
pub const KEY_102ND: u16 = 86;
pub const KEY_F11: u16 = 87;
pub const KEY_F12: u16 = 88;
pub const KEY_RIGHTCTRL: u16 = 97;
pub const KEY_KPSLASH: u16 = 98;
pub const KEY_SYSRQ: u16 = 99;
pub const KEY_RIGHTALT: u16 = 100;
pub const KEY_HOME: u16 = 102;
pub const KEY_UP: u16 = 103;
pub const KEY_PAGEUP: u16 = 104;
pub const KEY_LEFT: u16 = 105;
pub const KEY_RIGHT: u16 = 106;
pub const KEY_END: u16 = 107;
pub const KEY_DOWN: u16 = 108;
pub const KEY_PAGEDOWN: u16 = 109;
pub const KEY_INSERT: u16 = 110;
pub const KEY_DELETE: u16 = 111;
pub const KEY_MUTE: u16 = 113;
pub const KEY_VOLUMEDOWN: u16 = 114;
pub const KEY_VOLUMEUP: u16 = 115;
pub const KEY_POWER: u16 = 116;
pub const KEY_KPEQUAL: u16 = 117;
pub const KEY_KPENTER: u16 = 96;
pub const KEY_PAUSE: u16 = 119;
pub const KEY_LEFTMETA: u16 = 125;
pub const KEY_RIGHTMETA: u16 = 126;
pub const KEY_COMPOSE: u16 = 127;
pub const KEY_COMMA: u16 = 51;
pub const KEY_DOT: u16 = 52;
pub const KEY_SLASH: u16 = 53;
pub const KEY_SLEEP: u16 = 142;
pub const KEY_WAKEUP: u16 = 143;
pub const KEY_NEXTSONG: u16 = 163;
pub const KEY_PLAYPAUSE: u16 = 164;
pub const KEY_PREVIOUSSONG: u16 = 165;
pub const KEY_STOPCD: u16 = 166;
pub const KEY_F13: u16 = 183;

/// Linux key codes of the letter usages `keyboard::A` to `keyboard::Z` (QWERTY layout order
/// of the Linux key codes).
const LETTER_CODES: [u16; 26] = [
    KEY_A, // A
    48,    // B
    46,    // C
    32,    // D
    18,    // E
    33,    // F
    34,    // G
    35,    // H
    23,    // I
    36,    // J
    37,    // K
    38,    // L
    50,    // M
    49,    // N
    24,    // O
    25,    // P
    16,    // Q
    19,    // R
    31,    // S
    20,    // T
    22,    // U
    47,    // V
    17,    // W
    45,    // X
    21,    // Y
    44,    // Z
];

/// Linux key code of a Keyboard/Keypad page (0x07) usage, or `None` when unmapped.
pub fn keyboard_usage(usage: u8) -> Option<u16> {
    match usage {
        keyboard::A..=keyboard::Z => Some(LETTER_CODES[usize::from(usage - keyboard::A)]),
        keyboard::NUMBER_1..=keyboard::NUMBER_9 => {
            Some(KEY_1 + u16::from(usage - keyboard::NUMBER_1))
        }
        keyboard::NUMBER_0 => Some(KEY_0),
        keyboard::ENTER => Some(KEY_ENTER),
        keyboard::ESCAPE => Some(KEY_ESC),
        keyboard::BACKSPACE => Some(KEY_BACKSPACE),
        keyboard::TAB => Some(KEY_TAB),
        keyboard::SPACE => Some(KEY_SPACE),
        // Punctuation 0x2D to 0x38: minus, equal, brackets, backslash, non-US #/~,
        // semicolon, apostrophe, grave, comma, dot, slash.
        0x2D => Some(KEY_MINUS),
        0x2E => Some(KEY_EQUAL),
        0x2F => Some(KEY_LEFTBRACE),
        0x30 => Some(KEY_RIGHTBRACE),
        0x31 => Some(KEY_BACKSLASH),
        0x32 => Some(KEY_102ND),
        0x33 => Some(KEY_SEMICOLON),
        0x34 => Some(KEY_APOSTROPHE),
        0x35 => Some(KEY_GRAVE),
        0x36 => Some(KEY_COMMA),
        0x37 => Some(KEY_DOT),
        0x38 => Some(KEY_SLASH),
        keyboard::CAPS_LOCK => Some(KEY_CAPSLOCK),
        keyboard::F1..=keyboard::F10 => Some(KEY_F1 + u16::from(usage - keyboard::F1)),
        keyboard::F11 => Some(KEY_F11),
        keyboard::F12 => Some(KEY_F12),
        keyboard::PRINT_SCREEN => Some(KEY_SYSRQ),
        keyboard::SCROLL_LOCK => Some(KEY_SCROLLLOCK),
        keyboard::PAUSE => Some(KEY_PAUSE),
        keyboard::INSERT => Some(KEY_INSERT),
        keyboard::HOME => Some(KEY_HOME),
        keyboard::PAGE_UP => Some(KEY_PAGEUP),
        keyboard::DELETE => Some(KEY_DELETE),
        keyboard::END => Some(KEY_END),
        keyboard::PAGE_DOWN => Some(KEY_PAGEDOWN),
        keyboard::ARROW_RIGHT => Some(KEY_RIGHT),
        keyboard::ARROW_LEFT => Some(KEY_LEFT),
        keyboard::ARROW_DOWN => Some(KEY_DOWN),
        keyboard::ARROW_UP => Some(KEY_UP),
        // Keypad 0x53 to 0x67.
        0x53 => Some(KEY_NUMLOCK),
        0x54 => Some(KEY_KPSLASH),
        0x55 => Some(KEY_KPASTERISK),
        0x56 => Some(KEY_KPMINUS),
        0x57 => Some(KEY_KPPLUS),
        0x58 => Some(KEY_KPENTER),
        0x59 => Some(KEY_KP1),
        0x5A => Some(KEY_KP2),
        0x5B => Some(KEY_KP3),
        0x5C => Some(KEY_KP4),
        0x5D => Some(KEY_KP5),
        0x5E => Some(KEY_KP6),
        0x5F => Some(KEY_KP7),
        0x60 => Some(KEY_KP8),
        0x61 => Some(KEY_KP9),
        0x62 => Some(KEY_KP0),
        0x63 => Some(KEY_KPDOT),
        0x64 => Some(KEY_102ND),
        0x65 => Some(KEY_COMPOSE),
        0x67 => Some(KEY_KPEQUAL),
        keyboard::F13..=keyboard::F24 => Some(KEY_F13 + u16::from(usage - keyboard::F13)),
        0xE0..=0xE7 => Some(MODIFIER_KEY_CODES[usize::from(usage - 0xE0)]),
        _ => None,
    }
}

/// Linux key code of a Consumer page (0x0C) usage, or `None` when unmapped.
pub fn consumer_usage(usage: u16) -> Option<u16> {
    match usage {
        consumer::PLAY_PAUSE => Some(KEY_PLAYPAUSE),
        consumer::NEXT_TRACK => Some(KEY_NEXTSONG),
        consumer::PREVIOUS_TRACK => Some(KEY_PREVIOUSSONG),
        consumer::STOP => Some(KEY_STOPCD),
        consumer::VOLUME_UP => Some(KEY_VOLUMEUP),
        consumer::VOLUME_DOWN => Some(KEY_VOLUMEDOWN),
        consumer::MUTE => Some(KEY_MUTE),
        _ => None,
    }
}

/// Linux key code of a Generic Desktop system control usage, or `None` when unmapped.
pub fn system_usage(usage: u8) -> Option<u16> {
    match usage {
        system::POWER_DOWN => Some(KEY_POWER),
        system::SLEEP => Some(KEY_SLEEP),
        system::WAKE_UP => Some(KEY_WAKEUP),
        _ => None,
    }
}

/// Linux key code of each modifier bit, HID order: bit 0 left control to bit 7 right GUI.
pub const MODIFIER_KEY_CODES: [u16; 8] = [
    KEY_LEFTCTRL,
    KEY_LEFTSHIFT,
    KEY_LEFTALT,
    KEY_LEFTMETA,
    KEY_RIGHTCTRL,
    KEY_RIGHTSHIFT,
    KEY_RIGHTALT,
    KEY_RIGHTMETA,
];

/// Linux button code of each mouse button bit: bit 0 left, bit 1 right, bit 2 middle.
pub const MOUSE_BUTTON_CODES: [u16; 3] = [BTN_LEFT, BTN_RIGHT, BTN_MIDDLE];

/// Every key code this driver can emit, sorted and without duplicates, for `UI_SET_KEYBIT`
/// registration: all mapped keyboard usages, the consumer and system usages and the mouse
/// buttons.
pub fn all_key_codes() -> std::vec::Vec<u16> {
    let mut codes: std::vec::Vec<u16> = (0u8..=0xE7)
        .filter_map(keyboard_usage)
        .chain(
            [
                consumer::PLAY_PAUSE,
                consumer::NEXT_TRACK,
                consumer::PREVIOUS_TRACK,
                consumer::STOP,
                consumer::VOLUME_UP,
                consumer::VOLUME_DOWN,
                consumer::MUTE,
            ]
            .into_iter()
            .filter_map(consumer_usage),
        )
        .chain(
            [system::POWER_DOWN, system::SLEEP, system::WAKE_UP]
                .into_iter()
                .filter_map(system_usage),
        )
        .chain(MOUSE_BUTTON_CODES)
        .collect();
    codes.sort_unstable();
    codes.dedup();
    codes
}

#[cfg(test)]
mod tests {
    use super::*;
    use device::human_interface::Modifiers;

    #[test]
    fn letters_map_to_the_linux_qwerty_codes() {
        assert_eq!(keyboard_usage(keyboard::A), Some(KEY_A));
        assert_eq!(keyboard_usage(keyboard::B), Some(48));
        assert_eq!(keyboard_usage(keyboard::M), Some(50));
        assert_eq!(keyboard_usage(keyboard::Q), Some(16));
        assert_eq!(keyboard_usage(keyboard::Z), Some(44));
        for usage in keyboard::A..=keyboard::Z {
            assert!(keyboard_usage(usage).is_some());
        }
    }

    #[test]
    fn digits_and_named_keys_map_to_their_linux_codes() {
        assert_eq!(keyboard_usage(keyboard::NUMBER_1), Some(KEY_1));
        assert_eq!(keyboard_usage(keyboard::NUMBER_9), Some(KEY_1 + 8));
        assert_eq!(keyboard_usage(keyboard::NUMBER_0), Some(KEY_0));
        assert_eq!(keyboard_usage(keyboard::ENTER), Some(KEY_ENTER));
        assert_eq!(keyboard_usage(keyboard::ESCAPE), Some(KEY_ESC));
        assert_eq!(keyboard_usage(keyboard::BACKSPACE), Some(KEY_BACKSPACE));
        assert_eq!(keyboard_usage(keyboard::TAB), Some(KEY_TAB));
        assert_eq!(keyboard_usage(keyboard::SPACE), Some(KEY_SPACE));
        assert_eq!(keyboard_usage(keyboard::CAPS_LOCK), Some(KEY_CAPSLOCK));
        assert_eq!(keyboard_usage(keyboard::PRINT_SCREEN), Some(KEY_SYSRQ));
        assert_eq!(keyboard_usage(keyboard::SCROLL_LOCK), Some(KEY_SCROLLLOCK));
        assert_eq!(keyboard_usage(keyboard::PAUSE), Some(KEY_PAUSE));
        assert_eq!(keyboard_usage(keyboard::INSERT), Some(KEY_INSERT));
        assert_eq!(keyboard_usage(keyboard::HOME), Some(KEY_HOME));
        assert_eq!(keyboard_usage(keyboard::PAGE_UP), Some(KEY_PAGEUP));
        assert_eq!(keyboard_usage(keyboard::DELETE), Some(KEY_DELETE));
        assert_eq!(keyboard_usage(keyboard::END), Some(KEY_END));
        assert_eq!(keyboard_usage(keyboard::PAGE_DOWN), Some(KEY_PAGEDOWN));
        assert_eq!(keyboard_usage(keyboard::ARROW_RIGHT), Some(KEY_RIGHT));
        assert_eq!(keyboard_usage(keyboard::ARROW_LEFT), Some(KEY_LEFT));
        assert_eq!(keyboard_usage(keyboard::ARROW_DOWN), Some(KEY_DOWN));
        assert_eq!(keyboard_usage(keyboard::ARROW_UP), Some(KEY_UP));
        assert_eq!(keyboard_usage(keyboard::F1), Some(KEY_F1));
        assert_eq!(keyboard_usage(keyboard::F12), Some(KEY_F12));
        assert_eq!(keyboard_usage(keyboard::F13), Some(KEY_F13));
        assert_eq!(keyboard_usage(keyboard::F24), Some(KEY_F13 + 11));
    }

    #[test]
    fn punctuation_maps_to_its_linux_code() {
        assert_eq!(keyboard_usage(0x2D), Some(KEY_MINUS));
        assert_eq!(keyboard_usage(0x2E), Some(KEY_EQUAL));
        assert_eq!(keyboard_usage(0x2F), Some(KEY_LEFTBRACE));
        assert_eq!(keyboard_usage(0x30), Some(KEY_RIGHTBRACE));
        assert_eq!(keyboard_usage(0x31), Some(KEY_BACKSLASH));
        assert_eq!(keyboard_usage(0x32), Some(KEY_102ND));
        assert_eq!(keyboard_usage(0x33), Some(KEY_SEMICOLON));
        assert_eq!(keyboard_usage(0x34), Some(KEY_APOSTROPHE));
        assert_eq!(keyboard_usage(0x35), Some(KEY_GRAVE));
        assert_eq!(keyboard_usage(0x36), Some(KEY_COMMA));
        assert_eq!(keyboard_usage(0x37), Some(KEY_DOT));
        assert_eq!(keyboard_usage(0x38), Some(KEY_SLASH));
    }

    #[test]
    fn keypad_and_application_keys_map_to_their_linux_codes() {
        assert_eq!(keyboard_usage(0x53), Some(KEY_NUMLOCK));
        assert_eq!(keyboard_usage(0x54), Some(KEY_KPSLASH));
        assert_eq!(keyboard_usage(0x55), Some(KEY_KPASTERISK));
        assert_eq!(keyboard_usage(0x56), Some(KEY_KPMINUS));
        assert_eq!(keyboard_usage(0x57), Some(KEY_KPPLUS));
        assert_eq!(keyboard_usage(0x58), Some(KEY_KPENTER));
        assert_eq!(keyboard_usage(0x59), Some(KEY_KP1));
        assert_eq!(keyboard_usage(0x5A), Some(KEY_KP2));
        assert_eq!(keyboard_usage(0x5B), Some(KEY_KP3));
        assert_eq!(keyboard_usage(0x5C), Some(KEY_KP4));
        assert_eq!(keyboard_usage(0x5D), Some(KEY_KP5));
        assert_eq!(keyboard_usage(0x5E), Some(KEY_KP6));
        assert_eq!(keyboard_usage(0x5F), Some(KEY_KP7));
        assert_eq!(keyboard_usage(0x60), Some(KEY_KP8));
        assert_eq!(keyboard_usage(0x61), Some(KEY_KP9));
        assert_eq!(keyboard_usage(0x62), Some(KEY_KP0));
        assert_eq!(keyboard_usage(0x63), Some(KEY_KPDOT));
        assert_eq!(keyboard_usage(0x64), Some(KEY_102ND));
        assert_eq!(keyboard_usage(0x65), Some(KEY_COMPOSE));
        assert_eq!(keyboard_usage(0x67), Some(KEY_KPEQUAL));
    }

    #[test]
    fn unmapped_keyboard_usages_are_none() {
        for usage in [0x00, 0x01, 0x66, 0xE8, 0xFF] {
            assert_eq!(keyboard_usage(usage), None, "usage 0x{usage:02X}");
        }
    }

    #[test]
    fn modifier_usages_map_in_hid_order() {
        assert_eq!(keyboard_usage(0xE0), Some(KEY_LEFTCTRL));
        assert_eq!(keyboard_usage(0xE1), Some(KEY_LEFTSHIFT));
        assert_eq!(keyboard_usage(0xE2), Some(KEY_LEFTALT));
        assert_eq!(keyboard_usage(0xE3), Some(KEY_LEFTMETA));
        assert_eq!(keyboard_usage(0xE4), Some(KEY_RIGHTCTRL));
        assert_eq!(keyboard_usage(0xE5), Some(KEY_RIGHTSHIFT));
        assert_eq!(keyboard_usage(0xE6), Some(KEY_RIGHTALT));
        assert_eq!(keyboard_usage(0xE7), Some(KEY_RIGHTMETA));
        // The modifier bitmask bit n carries usage 0xE0 + n.
        for bit in 0u8..8 {
            let modifier = Modifiers::from_bits_retain(1 << bit);
            assert_eq!(
                MODIFIER_KEY_CODES[usize::from(bit)],
                keyboard_usage(0xE0 + bit).unwrap(),
                "modifier bit {bit}"
            );
            assert!(modifier.bits() == 1 << bit);
        }
    }

    #[test]
    fn consumer_and_system_usages_map_to_their_linux_codes() {
        assert_eq!(consumer_usage(consumer::PLAY_PAUSE), Some(KEY_PLAYPAUSE));
        assert_eq!(consumer_usage(consumer::NEXT_TRACK), Some(KEY_NEXTSONG));
        assert_eq!(
            consumer_usage(consumer::PREVIOUS_TRACK),
            Some(KEY_PREVIOUSSONG)
        );
        assert_eq!(consumer_usage(consumer::STOP), Some(KEY_STOPCD));
        assert_eq!(consumer_usage(consumer::VOLUME_UP), Some(KEY_VOLUMEUP));
        assert_eq!(consumer_usage(consumer::VOLUME_DOWN), Some(KEY_VOLUMEDOWN));
        assert_eq!(consumer_usage(consumer::MUTE), Some(KEY_MUTE));
        assert_eq!(consumer_usage(0x0001), None);
        assert_eq!(consumer_usage(0xFFFF), None);

        assert_eq!(system_usage(system::POWER_DOWN), Some(KEY_POWER));
        assert_eq!(system_usage(system::SLEEP), Some(KEY_SLEEP));
        assert_eq!(system_usage(system::WAKE_UP), Some(KEY_WAKEUP));
        assert_eq!(system_usage(0x80), None);
        assert_eq!(system_usage(0x84), None);
    }

    #[test]
    fn mouse_button_bits_map_to_button_codes() {
        assert_eq!(MOUSE_BUTTON_CODES[0], BTN_LEFT);
        assert_eq!(MOUSE_BUTTON_CODES[1], BTN_RIGHT);
        assert_eq!(MOUSE_BUTTON_CODES[2], BTN_MIDDLE);
    }

    #[test]
    fn every_mappable_usage_is_registered() {
        let registered = all_key_codes();
        let contains = |code: u16| registered.contains(&code);
        for usage in 0u8..=0xE7 {
            if let Some(code) = keyboard_usage(usage) {
                assert!(contains(code), "keyboard usage 0x{usage:02X}");
            }
        }
        for usage in [
            consumer::PLAY_PAUSE,
            consumer::NEXT_TRACK,
            consumer::PREVIOUS_TRACK,
            consumer::STOP,
            consumer::VOLUME_UP,
            consumer::VOLUME_DOWN,
            consumer::MUTE,
        ] {
            let code = consumer_usage(usage).unwrap();
            assert!(contains(code), "consumer 0x{usage:04X}");
        }
        for usage in [system::POWER_DOWN, system::SLEEP, system::WAKE_UP] {
            let code = system_usage(usage).unwrap();
            assert!(contains(code), "system 0x{usage:02X}");
        }
        for code in MOUSE_BUTTON_CODES {
            assert!(contains(code));
        }
        // Sorted and without duplicates, so registration is a set.
        for (index, code) in registered.iter().enumerate() {
            if index + 1 < registered.len() {
                assert!(
                    registered[index + 1] > *code,
                    "duplicate or unsorted {code}"
                );
            }
        }
    }
}
