//! A bare-metal Xila boot example for ESP32-S3, used by the QEMU smoke check.
#![cfg_attr(all(target_os = "none", target_arch = "xtensa"), no_std, no_main)]

#[cfg(all(target_os = "none", target_arch = "xtensa"))]
mod firmware;

#[cfg(not(all(target_os = "none", target_arch = "xtensa")))]
fn main() {
    panic!("This example requires an ESP32 bare-metal target.");
}
