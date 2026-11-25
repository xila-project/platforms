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
pub const KEY_BACKSPACE: u16 = 14;
pub const KEY_TAB: u16 = 15;
pub const KEY_ENTER: u16 = 28;
pub const KEY_LEFTCTRL: u16 = 29;
pub const KEY_LEFTSHIFT: u16 = 42;
pub const KEY_RIGHTSHIFT: u16 = 54;
pub const KEY_LEFTALT: u16 = 56;
pub const KEY_SPACE: u16 = 57;
pub const KEY_CAPSLOCK: u16 = 58;
pub const KEY_F1: u16 = 59;
pub const KEY_F10: u16 = 68;
pub const KEY_F11: u16 = 87;
pub const KEY_F12: u16 = 88;
pub const KEY_RIGHTCTRL: u16 = 97;
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
pub const KEY_PAUSE: u16 = 119;
pub const KEY_LEFTMETA: u16 = 125;
pub const KEY_RIGHTMETA: u16 = 126;
pub const KEY_SCROLLLOCK: u16 = 70;
pub const KEY_SLEEP: u16 = 142;
pub const KEY_WAKEUP: u16 = 143;
pub const KEY_NEXTSONG: u16 = 163;
pub const KEY_PLAYPAUSE: u16 = 164;
pub const KEY_PREVIOUSSONG: u16 = 165;
pub const KEY_STOPCD: u16 = 166;
pub const KEY_F13: u16 = 183;
pub const KEY_F24: u16 = 194;

/// Linux key codes of the letter usages `keyboard::A` to `keyboard::Z` (QWERTY layout order
/// of the Linux key codes).
const LETTER_CODES: [u16; 26] = [
    30, // A
    48, // B
    46, // C
    32, // D
    18, // E
    33, // F
    34, // G
    35, // H
    23, // I
    36, // J
    37, // K
    38, // L
    50, // M
    49, // N
    24, // O
    25, // P
    16, // Q
    19, // R
    31, // S
    20, // T
    22, // U
    47, // V
    17, // W
    45, // X
    21, // Y
    44, // Z
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

/// Key codes that are not letters, digits or function keys, registered with `UI_SET_KEYBIT`
/// together with the generated tables of [`all_key_codes`].
const BASE_KEY_CODES: [u16; 40] = [
    KEY_ESC,
    KEY_BACKSPACE,
    KEY_TAB,
    KEY_ENTER,
    KEY_LEFTCTRL,
    KEY_LEFTSHIFT,
    KEY_RIGHTSHIFT,
    KEY_LEFTALT,
    KEY_SPACE,
    KEY_CAPSLOCK,
    KEY_RIGHTCTRL,
    KEY_SYSRQ,
    KEY_RIGHTALT,
    KEY_HOME,
    KEY_UP,
    KEY_PAGEUP,
    KEY_LEFT,
    KEY_RIGHT,
    KEY_END,
    KEY_DOWN,
    KEY_PAGEDOWN,
    KEY_INSERT,
    KEY_DELETE,
    KEY_MUTE,
    KEY_VOLUMEDOWN,
    KEY_VOLUMEUP,
    KEY_POWER,
    KEY_PAUSE,
    KEY_LEFTMETA,
    KEY_RIGHTMETA,
    KEY_SCROLLLOCK,
    KEY_SLEEP,
    KEY_WAKEUP,
    KEY_NEXTSONG,
    KEY_PLAYPAUSE,
    KEY_PREVIOUSSONG,
    KEY_STOPCD,
    BTN_LEFT,
    BTN_RIGHT,
    BTN_MIDDLE,
];

/// Every key code this driver can emit, for `UI_SET_KEYBIT` registration.
pub fn all_key_codes() -> impl Iterator<Item = u16> {
    BASE_KEY_CODES
        .iter()
        .copied()
        .chain(LETTER_CODES.iter().copied())
        .chain(KEY_1..=(KEY_1 + 8))
        .chain(core::iter::once(KEY_0))
        .chain(KEY_F1..=KEY_F10)
        .chain([KEY_F11, KEY_F12])
        .chain(KEY_F13..=KEY_F24)
}

#[cfg(test)]
mod tests {
    use super::*;
    use device::human_interface::Modifiers;

    #[test]
    fn letters_map_to_the_linux_qwerty_codes() {
        assert_eq!(keyboard_usage(keyboard::A), Some(30));
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
        assert_eq!(keyboard_usage(keyboard::F24), Some(KEY_F24));
    }

    #[test]
    fn unmapped_keyboard_usages_are_none() {
        for usage in [0x00, 0x01, 0x2D, 0x38, 0x64, 0xE8, 0xFF] {
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
        let registered: std::vec::Vec<u16> = all_key_codes().collect();
        let contains = |code: u16| registered.contains(&code);
        for usage in keyboard::A..=keyboard::Z {
            let code = keyboard_usage(usage).unwrap();
            assert!(contains(code), "letter 0x{usage:02X}");
        }
        for usage in [
            keyboard::NUMBER_1,
            keyboard::NUMBER_0,
            keyboard::ENTER,
            keyboard::ESCAPE,
            keyboard::BACKSPACE,
            keyboard::TAB,
            keyboard::SPACE,
            keyboard::CAPS_LOCK,
            keyboard::F1,
            keyboard::F12,
            keyboard::PRINT_SCREEN,
            keyboard::SCROLL_LOCK,
            keyboard::PAUSE,
            keyboard::INSERT,
            keyboard::HOME,
            keyboard::PAGE_UP,
            keyboard::DELETE,
            keyboard::END,
            keyboard::PAGE_DOWN,
            keyboard::ARROW_RIGHT,
            keyboard::ARROW_LEFT,
            keyboard::ARROW_DOWN,
            keyboard::ARROW_UP,
            keyboard::F13,
            keyboard::F24,
        ] {
            let code = keyboard_usage(usage).unwrap();
            assert!(contains(code), "key 0x{usage:02X}");
        }
        for code in MODIFIER_KEY_CODES {
            assert!(contains(code));
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
        // No duplicates, so registration is a set.
        for (index, code) in registered.iter().enumerate() {
            assert!(!registered[index + 1..].contains(code), "duplicate {code}");
        }
    }
}
