//! Bare-metal Xila example for Espressif targets (ESP32-S3, ESP32-C6 and ESP32-P4).
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
