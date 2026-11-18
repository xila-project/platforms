//! Choosing pins for the peripheral adapters.
//!
//! Board pins are chosen by the board, not by this crate. The board builds each
//! esp-hal driver with its own pins and passes the driver to the adapter, for
//! example:
//!
//! ```text
//! let i2c = esp_hal::i2c::master::I2c::new(peripherals.I2C0, Config::default())?
//!     .with_sda(peripherals.GPIO4)
//!     .with_scl(peripherals.GPIO5);
//! let i2c = EspI2c::new(i2c);
//! ```
//!
//! esp-hal's trait bounds reject a pin that cannot carry a signal (for
//! example an output on an input-only pin), so such a mistake fails to compile.
//! What the bounds cannot express is recorded per chip in [`crate::chips`]:
//! tables generated from esp-hal's chip metadata. They leave out flash and USB
//! pins, mark input-only pins, and flag strapping, JTAG, UART0 console and
//! module PSRAM/octal pins with `LineFlags`.
//!
//! Every pin given to a peripheral driver must also be withheld from the GPIO
//! controller (`shared::pins_mask` in the mask passed to `Gpio::new`);
//! otherwise opening its `/devices/gpio/pin<N>` device reconfigures the pin and
//! breaks that peripheral.
//!
//! Strapping pins are sampled at reset only. Use one when the board's circuit
//! does not hold it at a level that changes the boot mode. ADC channel numbers
//! and the ADC2/Wi-Fi restriction are documented per chip by esp-hal; check
//! the module datasheet as well, since modules add constraints of their own
//! (PSRAM, 1.8 V flash supplies, onboard LEDs).
