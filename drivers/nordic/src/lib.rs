#![no_std]
#![cfg(any(all(target_arch = "arm", target_os = "none"), test))]

#[cfg(all(target_arch = "arm", target_os = "none"))]
pub mod executor;
pub mod flash;
pub mod gpio;
pub mod memory;
pub mod random;
#[cfg(all(target_arch = "arm", target_os = "none"))]
pub mod time;
pub mod usb_console;

pub extern crate memory as memory_exported;

#[cfg(all(test, not(target_os = "none")))]
extern crate std;
