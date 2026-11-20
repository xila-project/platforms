//! A bare-metal Xila boot example for ESP32-S3 and ESP32-C6, used by the QEMU smoke checks.
#![cfg_attr(
    all(
        target_os = "none",
        any(target_arch = "xtensa", target_arch = "riscv32")
    ),
    no_std,
    no_main
)]

#[cfg(all(
    target_os = "none",
    any(target_arch = "xtensa", target_arch = "riscv32")
))]
mod firmware;

#[cfg(not(all(
    target_os = "none",
    any(target_arch = "xtensa", target_arch = "riscv32")
)))]
fn main() {
    panic!("This example requires an ESP32 bare-metal target.");
}
