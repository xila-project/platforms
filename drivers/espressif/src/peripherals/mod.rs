//! Platform adapters that implement the portable peripheral contracts on esp-hal.

#[cfg(feature = "i2c")]
pub mod i2c;
pub mod pins;
#[cfg(feature = "spi")]
pub mod spi;
#[cfg(feature = "uart")]
pub mod uart;
