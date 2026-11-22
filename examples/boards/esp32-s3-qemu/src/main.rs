//! Espressif QEMU's ESP32-S3 machine: the emulator smoke test.
#![cfg_attr(all(target_os = "none", target_arch = "xtensa"), no_std, no_main)]

#[cfg(all(target_os = "none", target_arch = "xtensa"))]
mod firmware;

#[cfg(not(all(target_os = "none", target_arch = "xtensa")))]
fn main() {
    panic!("This board crate builds for its Espressif bare-metal target only.");
}
