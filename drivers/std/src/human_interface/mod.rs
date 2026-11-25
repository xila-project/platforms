//! Hosted lower halves of the human interface emulation class.
//!
//! On Linux, [`uinput::UinputController`] implements
//! `peripherals::human_interface::HumanInterfaceController` over the `uinput` interface
//! (`/dev/uinput`), so the emulation protocol of `device::human_interface` can be tested on
//! a computer without hardware. The native example mounts it through the upper half
//! (`drivers_peripherals::human_interface::mount_human_interface`) at
//! `/devices/human_interface/emulation`.
//!
//! Later USB (`embassy-usb` on the ESP32-S3 and the nRF52840) and Bluetooth Low Energy
//! (`esp-radio` / `nrf-sdc` with `trouble-host`) lower halves implement the same trait and
//! share `peripherals::human_interface::REPORT_DESCRIPTOR`.

mod mapping;
pub mod uinput;

pub use uinput::UinputController;
