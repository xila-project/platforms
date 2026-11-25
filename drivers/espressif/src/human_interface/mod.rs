//! Human interface emulation lower half over USB, on the ESP32-S3 (feature
//! `usb-human-interface`).
//!
//! The ESP32-S3 has one USB PHY, shared by the USB Serial/JTAG controller and the
//! USB OTG controller, so a board that wants both the console and the human
//! interface emulation on its USB port provides them as one composite device:
//! CDC-ACM (the console) and HID (keyboard, media remote, mouse and system control
//! toward the connected computer). [`UsbHumanInterface::new`] builds that device
//! over `embassy-usb` on esp-hal's USB OTG driver. The console port it returns goes
//! to the firmware's `Board`, and the controller it returns implements
//! `peripherals::human_interface::HumanInterfaceController`, which the upper half
//! mounts at `/devices/human_interface/emulation`.
//!
//! `logic` holds the transport-independent parts, the bounded report queue between
//! `send_report` and the USB runner task, the report framing (the identifier byte in
//! front of the report bytes) and the parsing of the keyboard LED output report. It
//! is host-tested by `tests/human_interface.rs`.

mod logic;

mod usb;

pub(crate) use usb::take_runner;
pub use usb::{
    UsbCompositeRunner, UsbHumanInterface, UsbHumanInterfaceConfig, UsbHumanInterfaceController,
};
