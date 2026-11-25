//! Mapping from the HID usages of `device::human_interface` to Linux input event codes
//! (`linux/input-event-codes.h`). Usages without a mapping are skipped by the controller.

use device::human_interface::{consumer, system};

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

// Additional Linux key codes used by the kernel keyboard table below.
pub const KEY_2: u16 = 3;
pub const KEY_3: u16 = 4;
pub const KEY_4: u16 = 5;
pub const KEY_5: u16 = 6;
pub const KEY_6: u16 = 7;
pub const KEY_7: u16 = 8;
pub const KEY_8: u16 = 9;
pub const KEY_9: u16 = 10;
pub const KEY_Q: u16 = 16;
pub const KEY_W: u16 = 17;
pub const KEY_E: u16 = 18;
pub const KEY_R: u16 = 19;
pub const KEY_T: u16 = 20;
pub const KEY_Y: u16 = 21;
pub const KEY_U: u16 = 22;
pub const KEY_I: u16 = 23;
pub const KEY_O: u16 = 24;
pub const KEY_P: u16 = 25;
pub const KEY_S: u16 = 31;
pub const KEY_D: u16 = 32;
pub const KEY_F: u16 = 33;
pub const KEY_G: u16 = 34;
pub const KEY_H: u16 = 35;
pub const KEY_J: u16 = 36;
pub const KEY_K: u16 = 37;
pub const KEY_L: u16 = 38;
pub const KEY_Z: u16 = 44;
pub const KEY_X: u16 = 45;
pub const KEY_C: u16 = 46;
pub const KEY_V: u16 = 47;
pub const KEY_B: u16 = 48;
pub const KEY_N: u16 = 49;
pub const KEY_M: u16 = 50;
pub const KEY_F2: u16 = 60;
pub const KEY_F3: u16 = 61;
pub const KEY_F4: u16 = 62;
pub const KEY_F5: u16 = 63;
pub const KEY_F6: u16 = 64;
pub const KEY_F7: u16 = 65;
pub const KEY_F8: u16 = 66;
pub const KEY_F9: u16 = 67;
pub const KEY_F10: u16 = 68;
pub const KEY_ZENKAKUHANKAKU: u16 = 85;
pub const KEY_RO: u16 = 89;
pub const KEY_KATAKANA: u16 = 90;
pub const KEY_HIRAGANA: u16 = 91;
pub const KEY_HENKAN: u16 = 92;
pub const KEY_KATAKANAHIRAGANA: u16 = 93;
pub const KEY_MUHENKAN: u16 = 94;
pub const KEY_KPJPCOMMA: u16 = 95;
pub const KEY_KPCOMMA: u16 = 121;
pub const KEY_HANGEUL: u16 = 122;
pub const KEY_HANJA: u16 = 123;
pub const KEY_YEN: u16 = 124;
pub const KEY_STOP: u16 = 128;
pub const KEY_AGAIN: u16 = 129;
pub const KEY_PROPS: u16 = 130;
pub const KEY_UNDO: u16 = 131;
pub const KEY_FRONT: u16 = 132;
pub const KEY_COPY: u16 = 133;
pub const KEY_OPEN: u16 = 134;
pub const KEY_PASTE: u16 = 135;
pub const KEY_FIND: u16 = 136;
pub const KEY_CUT: u16 = 137;
pub const KEY_HELP: u16 = 138;
pub const KEY_CALC: u16 = 140;
pub const KEY_WWW: u16 = 150;
pub const KEY_COFFEE: u16 = 152;
pub const KEY_BACK: u16 = 158;
pub const KEY_FORWARD: u16 = 159;
pub const KEY_EJECTCD: u16 = 161;
pub const KEY_REFRESH: u16 = 173;
pub const KEY_EDIT: u16 = 176;
pub const KEY_SCROLLUP: u16 = 177;
pub const KEY_SCROLLDOWN: u16 = 178;
pub const KEY_KPLEFTPAREN: u16 = 179;
pub const KEY_KPRIGHTPAREN: u16 = 180;
pub const KEY_F14: u16 = 184;
pub const KEY_F15: u16 = 185;
pub const KEY_F16: u16 = 186;
pub const KEY_F17: u16 = 187;
pub const KEY_F18: u16 = 188;
pub const KEY_F19: u16 = 189;
pub const KEY_F20: u16 = 190;
pub const KEY_F21: u16 = 191;
pub const KEY_F22: u16 = 192;
pub const KEY_F23: u16 = 193;
pub const KEY_F24: u16 = 194;

/// HID Keyboard/Keypad page (0x07) usage to Linux key code. Mirrors the
/// `hid_keyboard[]` table of the Linux kernel (`drivers/hid/hid-input.c` in
/// `torvalds/linux`): `0` marks a usage the kernel does not map.
const KEYBOARD_USAGE_CODES: [u16; 256] = [
    0,
    0,
    0,
    0,
    KEY_A,
    KEY_B,
    KEY_C,
    KEY_D,
    KEY_E,
    KEY_F,
    KEY_G,
    KEY_H,
    KEY_I,
    KEY_J,
    KEY_K,
    KEY_L, // 0x00..=0x0F
    KEY_M,
    KEY_N,
    KEY_O,
    KEY_P,
    KEY_Q,
    KEY_R,
    KEY_S,
    KEY_T,
    KEY_U,
    KEY_V,
    KEY_W,
    KEY_X,
    KEY_Y,
    KEY_Z,
    KEY_1,
    KEY_2, // 0x10..=0x1F
    KEY_3,
    KEY_4,
    KEY_5,
    KEY_6,
    KEY_7,
    KEY_8,
    KEY_9,
    KEY_0,
    KEY_ENTER,
    KEY_ESC,
    KEY_BACKSPACE,
    KEY_TAB,
    KEY_SPACE,
    KEY_MINUS,
    KEY_EQUAL,
    KEY_LEFTBRACE, // 0x20..=0x2F
    KEY_RIGHTBRACE,
    KEY_BACKSLASH,
    KEY_BACKSLASH,
    KEY_SEMICOLON,
    KEY_APOSTROPHE,
    KEY_GRAVE,
    KEY_COMMA,
    KEY_DOT,
    KEY_SLASH,
    KEY_CAPSLOCK,
    KEY_F1,
    KEY_F2,
    KEY_F3,
    KEY_F4,
    KEY_F5,
    KEY_F6, // 0x30..=0x3F
    KEY_F7,
    KEY_F8,
    KEY_F9,
    KEY_F10,
    KEY_F11,
    KEY_F12,
    KEY_SYSRQ,
    KEY_SCROLLLOCK,
    KEY_PAUSE,
    KEY_INSERT,
    KEY_HOME,
    KEY_PAGEUP,
    KEY_DELETE,
    KEY_END,
    KEY_PAGEDOWN,
    KEY_RIGHT, // 0x40..=0x4F
    KEY_LEFT,
    KEY_DOWN,
    KEY_UP,
    KEY_NUMLOCK,
    KEY_KPSLASH,
    KEY_KPASTERISK,
    KEY_KPMINUS,
    KEY_KPPLUS,
    KEY_KPENTER,
    KEY_KP1,
    KEY_KP2,
    KEY_KP3,
    KEY_KP4,
    KEY_KP5,
    KEY_KP6,
    KEY_KP7, // 0x50..=0x5F
    KEY_KP8,
    KEY_KP9,
    KEY_KP0,
    KEY_KPDOT,
    KEY_102ND,
    KEY_COMPOSE,
    KEY_POWER,
    KEY_KPEQUAL,
    KEY_F13,
    KEY_F14,
    KEY_F15,
    KEY_F16,
    KEY_F17,
    KEY_F18,
    KEY_F19,
    KEY_F20, // 0x60..=0x6F
    KEY_F21,
    KEY_F22,
    KEY_F23,
    KEY_F24,
    KEY_OPEN,
    KEY_HELP,
    KEY_PROPS,
    KEY_FRONT,
    KEY_STOP,
    KEY_AGAIN,
    KEY_UNDO,
    KEY_CUT,
    KEY_COPY,
    KEY_PASTE,
    KEY_FIND,
    KEY_MUTE, // 0x70..=0x7F
    KEY_VOLUMEUP,
    KEY_VOLUMEDOWN,
    0,
    0,
    0,
    KEY_KPCOMMA,
    0,
    KEY_RO,
    KEY_KATAKANAHIRAGANA,
    KEY_YEN,
    KEY_HENKAN,
    KEY_MUHENKAN,
    KEY_KPJPCOMMA,
    0,
    0,
    0, // 0x80..=0x8F
    KEY_HANGEUL,
    KEY_HANJA,
    KEY_KATAKANA,
    KEY_HIRAGANA,
    KEY_ZENKAKUHANKAKU,
    0,
    0,
    0,
    0,
    0,
    0,
    0,
    KEY_DELETE,
    0,
    0,
    0, // 0x90..=0x9F
    0,
    0,
    0,
    0,
    0,
    0,
    0,
    0,
    0,
    0,
    0,
    0,
    0,
    0,
    0,
    0, // 0xA0..=0xAF
    0,
    0,
    0,
    0,
    0,
    0,
    KEY_KPLEFTPAREN,
    KEY_KPRIGHTPAREN,
    0,
    0,
    0,
    0,
    0,
    0,
    0,
    0, // 0xB0..=0xBF
    0,
    0,
    0,
    0,
    0,
    0,
    0,
    0,
    0,
    0,
    0,
    0,
    0,
    0,
    0,
    0, // 0xC0..=0xCF
    0,
    0,
    0,
    0,
    0,
    0,
    0,
    0,
    KEY_DELETE,
    0,
    0,
    0,
    0,
    0,
    0,
    0, // 0xD0..=0xDF
    KEY_LEFTCTRL,
    KEY_LEFTSHIFT,
    KEY_LEFTALT,
    KEY_LEFTMETA,
    KEY_RIGHTCTRL,
    KEY_RIGHTSHIFT,
    KEY_RIGHTALT,
    KEY_RIGHTMETA,
    KEY_PLAYPAUSE,
    KEY_STOPCD,
    KEY_PREVIOUSSONG,
    KEY_NEXTSONG,
    KEY_EJECTCD,
    KEY_VOLUMEUP,
    KEY_VOLUMEDOWN,
    KEY_MUTE, // 0xE0..=0xEF
    KEY_WWW,
    KEY_BACK,
    KEY_FORWARD,
    KEY_STOP,
    KEY_FIND,
    KEY_SCROLLUP,
    KEY_SCROLLDOWN,
    KEY_EDIT,
    KEY_SLEEP,
    KEY_COFFEE,
    KEY_REFRESH,
    KEY_CALC,
    0,
    0,
    0,
    0, // 0xF0..=0xFF
];

/// Linux key code of a Keyboard/Keypad page (0x07) usage, or `None` when the
/// kernel does not map the usage.
pub fn keyboard_usage(usage: u8) -> Option<u16> {
    match KEYBOARD_USAGE_CODES[usize::from(usage)] {
        0 => None,
        code => Some(code),
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
    let mut codes: std::vec::Vec<u16> = (0u8..=0xFF)
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
    use device::human_interface::{Modifiers, keyboard};

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
        // The kernel maps 0x31 and 0x32 to the same code; 0x64 (Non-US pipe)
        // is the usage mapped to KEY_102ND.
        assert_eq!(keyboard_usage(0x32), Some(KEY_BACKSLASH));
        assert_eq!(keyboard_usage(0x64), Some(KEY_102ND));
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
        // The kernel maps the keypad comma 0x66 to KEY_POWER.
        assert_eq!(keyboard_usage(0x66), Some(KEY_POWER));
    }

    #[test]
    fn unmapped_keyboard_usages_are_none() {
        for usage in [0x00, 0x01, 0x95, 0x9D, 0xFC, 0xFF] {
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
            assert_eq!(
                keyboard_usage(0xE0 + bit),
                Some(MODIFIER_KEY_CODES[usize::from(bit)])
            );
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
        for usage in 0u8..=0xFF {
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
