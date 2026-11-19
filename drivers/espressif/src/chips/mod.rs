//! GPIO line tables, one per chip, generated from esp-hal's chip metadata by
//! `tools/dev/generate_espressif_pins.py`.
//!
//! `LINES` is the table of the selected chip. Use `crate::shared::reserved_mask`
//! to withhold flagged lines, plus the pins the board's own drivers use.

#[cfg(all(feature = "esp32", feature = "gpio"))]
mod esp32;
#[cfg(all(feature = "esp32", feature = "gpio"))]
pub use esp32::LINES;

#[cfg(all(feature = "esp32c2", feature = "gpio"))]
mod esp32_c2;
#[cfg(all(feature = "esp32c2", feature = "gpio"))]
pub use esp32_c2::LINES;

#[cfg(all(feature = "esp32c3", feature = "gpio"))]
mod esp32_c3;
#[cfg(all(feature = "esp32c3", feature = "gpio"))]
pub use esp32_c3::LINES;

#[cfg(all(feature = "esp32c5", feature = "gpio"))]
mod esp32_c5;
#[cfg(all(feature = "esp32c5", feature = "gpio"))]
pub use esp32_c5::LINES;

#[cfg(all(feature = "esp32c6", feature = "gpio"))]
mod esp32_c6;
#[cfg(all(feature = "esp32c6", feature = "gpio"))]
pub use esp32_c6::LINES;

#[cfg(all(feature = "esp32c61", feature = "gpio"))]
mod esp32_c61;
#[cfg(all(feature = "esp32c61", feature = "gpio"))]
pub use esp32_c61::LINES;

#[cfg(all(feature = "esp32h2", feature = "gpio"))]
mod esp32_h2;
#[cfg(all(feature = "esp32h2", feature = "gpio"))]
pub use esp32_h2::LINES;

#[cfg(all(feature = "esp32p4", feature = "gpio"))]
mod esp32_p4;
#[cfg(all(feature = "esp32p4", feature = "gpio"))]
pub use esp32_p4::LINES;

#[cfg(all(feature = "esp32s2", feature = "gpio"))]
mod esp32_s2;
#[cfg(all(feature = "esp32s2", feature = "gpio"))]
pub use esp32_s2::LINES;

#[cfg(all(feature = "esp32s3", feature = "gpio"))]
mod esp32_s3;
#[cfg(all(feature = "esp32s3", feature = "gpio"))]
pub use esp32_s3::LINES;

#[cfg(all(feature = "esp32s31", feature = "gpio"))]
mod esp32_s31;
#[cfg(all(feature = "esp32s31", feature = "gpio"))]
pub use esp32_s31::LINES;
