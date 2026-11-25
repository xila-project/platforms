//! Ignored hardware test for the Linux `uinput` human interface controller.
//!
//! Run it on a computer where `/dev/uinput` is writable:
//!
//! ```sh
//! cargo test -p xila-drivers-std --features human-interface -- --ignored --nocapture uinput_hardware
//! ```
//!
//! Then, during the start delay, open the printed event device in another
//! terminal with `sudo evtest /dev/input/eventN`. `evtest` only lists the devices
//! that exist when it starts.
//!
//! The test opens the real `/dev/uinput`, prints the created device and its
//! `/dev/input/event*` path, waits 10 seconds so the device can be opened in
//! `evtest`, then sends harmless reports with 300 ms between steps and a printed
//! line for each: keyboard F24 press and release, Caps Lock press and release
//! (then the keyboard indicators are read and printed, then Caps Lock again to
//! restore the previous state), consumer Volume Up then Volume Down (net zero),
//! mouse x +10 then -10, horizontal pan +1 then -1. It never sends anything that
//! types text, opens programs or changes the power state. Without write access
//! to `/dev/uinput` the test prints the error and returns without failing.

#![cfg(all(
    feature = "human-interface",
    target_os = "linux",
    any(
        target_arch = "x86",
        target_arch = "x86_64",
        target_arch = "arm",
        target_arch = "aarch64",
        target_arch = "riscv32",
        target_arch = "riscv64"
    )
))]

use std::fs;
use std::thread::sleep;
use std::time::Duration;

use drivers_std::human_interface::UinputController;
use peripherals::human_interface::{
    HumanInterfaceController, REPORT_IDENTIFIER_CONSUMER, REPORT_IDENTIFIER_KEYBOARD,
    REPORT_IDENTIFIER_MOUSE,
};

/// HID Keyboard/Keypad page (0x07) usages used below.
const F24: u8 = 0x73;
const CAPS_LOCK: u8 = 0x39;

/// HID Consumer page (0x0C) usages used below.
const VOLUME_UP: u16 = 0x00E9;
const VOLUME_DOWN: u16 = 0x00EA;

/// Name of the virtual input device, as registered with `UI_DEV_SETUP`.
const DEVICE_NAME: &str = "Xila human interface emulation";

/// Delay between two steps, so every event is visible in `evtest`.
const STEP_DELAY: Duration = Duration::from_millis(300);

/// Keyboard report bytes without the identifier byte: modifiers, reserved, six keys.
fn keyboard_report(modifiers: u8, keys: [u8; 6]) -> [u8; 8] {
    let mut report = [0u8; 8];
    report[0] = modifiers;
    report[2..].copy_from_slice(&keys);
    report
}

/// Print one step, send its report and wait.
fn step(controller: &mut UinputController, description: &str, identifier: u8, report: &[u8]) {
    println!("--- {description}");
    controller
        .send_report(identifier, report)
        .unwrap_or_else(|error| panic!("sending {description} failed: {error}"));
    sleep(STEP_DELAY);
}

/// Find the `/dev/input/event*` node of the virtual device, from
/// `/sys/devices/virtual/input/*/name`.
fn event_device_path() -> Option<String> {
    for entry in fs::read_dir("/sys/devices/virtual/input").ok()? {
        let entry = entry.ok()?;
        let name = fs::read_to_string(entry.path().join("name")).ok()?;
        if name.trim() == DEVICE_NAME {
            for child in fs::read_dir(entry.path()).ok()? {
                let child = child.ok()?;
                if let Some(name) = child.file_name().to_str()
                    && name.starts_with("event")
                {
                    return Some(format!("/dev/input/{name}"));
                }
            }
        }
    }
    None
}

#[test]
#[ignore = "needs write access to /dev/uinput and a human watching evtest"]
fn uinput_hardware() {
    let mut controller = match UinputController::open() {
        Ok(controller) => controller,
        Err(error) => {
            println!("SKIPPED: cannot open /dev/uinput: {error}");
            return;
        }
    };
    println!("created virtual input device \"{DEVICE_NAME}\"");
    match event_device_path() {
        Some(path) => println!("event device: {path} (select it in evtest)"),
        None => println!("no event device found under /sys/devices/virtual/input yet"),
    }
    println!("waiting 10 seconds: open the event device with sudo evtest now...");
    sleep(Duration::from_secs(10));

    // Keyboard F24 press and release (unbound on desktops, harmless).
    step(
        &mut controller,
        "keyboard F24 press",
        REPORT_IDENTIFIER_KEYBOARD,
        &keyboard_report(0, [F24, 0, 0, 0, 0, 0]),
    );
    step(
        &mut controller,
        "keyboard release all",
        REPORT_IDENTIFIER_KEYBOARD,
        &keyboard_report(0, [0; 6]),
    );

    // Caps Lock press and release, then read the keyboard indicators the computer
    // sent back, then Caps Lock again to restore the previous state.
    step(
        &mut controller,
        "keyboard Caps Lock press",
        REPORT_IDENTIFIER_KEYBOARD,
        &keyboard_report(0, [CAPS_LOCK, 0, 0, 0, 0, 0]),
    );
    step(
        &mut controller,
        "keyboard release all",
        REPORT_IDENTIFIER_KEYBOARD,
        &keyboard_report(0, [0; 6]),
    );
    sleep(Duration::from_millis(500));
    println!(
        "keyboard indicators after Caps Lock: {:#04x}",
        controller.indicators()
    );
    step(
        &mut controller,
        "keyboard Caps Lock press (restore)",
        REPORT_IDENTIFIER_KEYBOARD,
        &keyboard_report(0, [CAPS_LOCK, 0, 0, 0, 0, 0]),
    );
    step(
        &mut controller,
        "keyboard release all",
        REPORT_IDENTIFIER_KEYBOARD,
        &keyboard_report(0, [0; 6]),
    );

    // Consumer Volume Up then Volume Down (net zero).
    step(
        &mut controller,
        "consumer Volume Up press",
        REPORT_IDENTIFIER_CONSUMER,
        &VOLUME_UP.to_le_bytes(),
    );
    step(
        &mut controller,
        "consumer release",
        REPORT_IDENTIFIER_CONSUMER,
        &0u16.to_le_bytes(),
    );
    step(
        &mut controller,
        "consumer Volume Down press",
        REPORT_IDENTIFIER_CONSUMER,
        &VOLUME_DOWN.to_le_bytes(),
    );
    step(
        &mut controller,
        "consumer release",
        REPORT_IDENTIFIER_CONSUMER,
        &0u16.to_le_bytes(),
    );

    // Mouse x +10 then -10.
    step(
        &mut controller,
        "mouse x +10",
        REPORT_IDENTIFIER_MOUSE,
        &[0, 10, 0, 0, 0],
    );
    step(
        &mut controller,
        "mouse x -10",
        REPORT_IDENTIFIER_MOUSE,
        &[0, (-10i8) as u8, 0, 0, 0],
    );

    // Horizontal pan +1 then -1.
    step(
        &mut controller,
        "mouse horizontal pan +1",
        REPORT_IDENTIFIER_MOUSE,
        &[0, 0, 0, 0, 1],
    );
    step(
        &mut controller,
        "mouse horizontal pan -1",
        REPORT_IDENTIFIER_MOUSE,
        &[0, 0, 0, 0, (-1i8) as u8],
    );

    // Release everything.
    step(
        &mut controller,
        "keyboard release all",
        REPORT_IDENTIFIER_KEYBOARD,
        &keyboard_report(0, [0; 6]),
    );
    step(
        &mut controller,
        "consumer release",
        REPORT_IDENTIFIER_CONSUMER,
        &0u16.to_le_bytes(),
    );
    step(
        &mut controller,
        "mouse buttons release",
        REPORT_IDENTIFIER_MOUSE,
        &[0, 0, 0, 0, 0],
    );

    println!("done, dropping the device");
    drop(controller);
}
