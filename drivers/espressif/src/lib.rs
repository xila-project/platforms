//! Espressif platform drivers over esp-hal, for the whole ESP32 family.
//!
//! Select the chip with exactly one Cargo feature (`esp32`, `esp32c2`, `esp32c3`,
//! `esp32c5`, `esp32c6`, `esp32c61`, `esp32h2`, `esp32p4`, `esp32s2`, `esp32s3`
//! or `esp32s31`). Without one, or on a host target, the crate is empty.
//!
//! The optional `console`, `executor`, `power`, `random` and `time` features
//! provide the shell's runtime drivers. Initialize the time driver before
//! starting the executor. Run the executor, console and timer on the same core.
//!
//! The optional `executable-memory` feature provides executable memory for the
//! native loader on the ESP32-C5, C6, C61, H2 and P4. The other chips cannot run
//! native images: the `executable_memory` module documents why.
//!
//! # Firmware requirements
//!
//! - **Critical section**: the drivers and memory manager use `critical-section`,
//!   so the firmware must link an implementation (for example by enabling
//!   `esp-hal`'s `rt` feature).
//! - **Atomic compare-and-swap on ESP32-C2, ESP32-C3 and ESP32-S2**: Xila Core
//!   uses `portable-atomic` / `portable-atomic-util` without selecting a backend.
//!   `esp-hal`'s `esp32c2`, `esp32c3` and `esp32s2` chip features already enable
//!   `portable-atomic/unsafe-assume-single-core`, so firmware using this crate
//!   gets a working backend automatically and must not also enable
//!   `portable-atomic/critical-section` (which `portable-atomic` rejects when
//!   combined with `unsafe-assume-single-core`).
#![no_std]
// The executor's idle sequence uses Xtensa inline assembly, which is still
// unstable; the `esp` toolchain allows it, as for esp-hal and esp-sync.
#![cfg_attr(
    all(target_arch = "xtensa", feature = "executor"),
    feature(asm_experimental_arch)
)]
#![cfg(all(
    any(target_arch = "xtensa", target_arch = "riscv32"),
    any(
        feature = "esp32",
        feature = "esp32c2",
        feature = "esp32c3",
        feature = "esp32c5",
        feature = "esp32c6",
        feature = "esp32c61",
        feature = "esp32h2",
        feature = "esp32p4",
        feature = "esp32s2",
        feature = "esp32s3",
        feature = "esp32s31",
    )
))]

const SELECTED_CHIPS: u8 = cfg!(feature = "esp32") as u8
    + cfg!(feature = "esp32c2") as u8
    + cfg!(feature = "esp32c3") as u8
    + cfg!(feature = "esp32c5") as u8
    + cfg!(feature = "esp32c6") as u8
    + cfg!(feature = "esp32c61") as u8
    + cfg!(feature = "esp32h2") as u8
    + cfg!(feature = "esp32p4") as u8
    + cfg!(feature = "esp32s2") as u8
    + cfg!(feature = "esp32s3") as u8
    + cfg!(feature = "esp32s31") as u8;

const _: () = assert!(
    SELECTED_CHIPS <= 1,
    "several Espressif chip features are enabled; enable exactly one"
);

pub mod chips;
#[cfg(feature = "console")]
pub mod console;
#[cfg(all(
    feature = "executable-memory",
    any(
        feature = "esp32c5",
        feature = "esp32c6",
        feature = "esp32c61",
        feature = "esp32h2",
        feature = "esp32p4",
    )
))]
pub mod executable_memory;
#[cfg(feature = "executor")]
pub mod executor;
pub mod memory;
#[cfg(feature = "power")]
pub mod power;
#[cfg(feature = "random")]
pub mod random;
#[cfg(feature = "time")]
pub mod time;

#[cfg(feature = "gpio")]
pub mod shared;

pub mod peripherals;

pub extern crate memory as memory_exported;
