//! Wireless-Tag's WT32-SC01 Plus (ESP32-S3, 16 MB flash, 2 MB PSRAM).
#![cfg_attr(all(target_os = "none", target_arch = "xtensa"), no_std, no_main)]

#[cfg(all(target_os = "none", target_arch = "xtensa"))]
extern crate alloc;

#[cfg(all(target_os = "none", target_arch = "xtensa"))]
mod firmware;

#[cfg(not(all(target_os = "none", target_arch = "xtensa")))]
fn main() {
    panic!("This board crate builds for its Espressif bare-metal target only.");
}
