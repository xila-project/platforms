//! Espressif platform drivers over esp-hal, for the whole ESP32 family.
//!
//! Select the chip with exactly one Cargo feature (`esp32`, `esp32c2`, `esp32c3`,
//! `esp32c5`, `esp32c6`, `esp32c61`, `esp32h2`, `esp32p4`, `esp32s2`, `esp32s3`
//! or `esp32s31`). Without one, or on a host target, the crate is empty.
#![no_std]
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

#[cfg(all(feature = "gpio", not(target_has_atomic = "ptr")))]
compile_error!(
    "the `gpio` feature needs Xila Core on a chip with atomic compare-and-swap; ESP32-C2, \
     ESP32-C3 and ESP32-S2 lack it (https://github.com/xila-project/core/issues/649). \
     Disable default features and enable `i2c`, `spi` or `uart` instead."
);

pub mod chips;
pub mod memory;

#[cfg(all(feature = "gpio", target_has_atomic = "ptr"))]
pub mod shared;

pub mod peripherals;

pub extern crate memory as memory_exported;
