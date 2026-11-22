//! Espressif's ESP32-P4X-Function-EV-Board (chip revision v3.0 or later, 16 MB flash).
#![cfg_attr(all(target_os = "none", target_arch = "riscv32"), no_std, no_main)]

#[cfg(all(target_os = "none", target_arch = "riscv32"))]
mod firmware;

#[cfg(not(all(target_os = "none", target_arch = "riscv32")))]
fn main() {
    panic!("This board crate builds for its Espressif bare-metal target only.");
}
