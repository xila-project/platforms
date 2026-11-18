#![no_std]
#![cfg(any(all(target_arch = "arm", target_os = "none"), test))]

#[cfg(all(target_arch = "arm", target_os = "none"))]
pub mod executor;
pub mod flash;
pub mod gpio;
#[cfg(all(target_arch = "arm", target_os = "none"))]
pub mod memory;
pub mod random;
#[cfg(all(target_arch = "arm", target_os = "none"))]
pub mod time;
pub mod usb_console;

#[cfg(all(target_arch = "arm", target_os = "none"))]
pub extern crate memory as memory_exported;
