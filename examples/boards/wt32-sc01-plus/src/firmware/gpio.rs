//! The GPIO devices of the WT32-SC01 Plus: `/devices/gpio/pin<number>` for the
//! six pins of the Extended IO connector.
//!
//! Source: Wireless-Tag WT32-SC01 Plus datasheet V1.3, table 2 "Extended IO
//! Interface" (connector J5, EXT_IO1 to EXT_IO6) and the schematic of the same
//! document (block "EXT CONN"):
//!
//! | Signal | GPIO |
//! |---|---|
//! | EXT_IO1 to EXT_IO4 | 10, 11, 12, 13 |
//! | EXT_IO5, EXT_IO6 | 14, 21 |
//!
//! The datasheet lists no other GPIO signal on a connector that a board
//! function does not use, so the controller exposes these pins only. It
//! withholds every other pin: configuring a device can then never reconfigure
//! a pin of the display, the touch panel, the backlight, the USB console, the
//! flash, the PSRAM, the microSD slot or the RS-485 transceiver.
//! [`USED_BY_BOARD`] collects those pins and the build fails when one of them
//! is also in [`EXPOSED`].

use drivers_espressif::{
    chips::LINES,
    shared::{Gpio, pins_mask},
};

/// Pins of the Extended IO connector, EXT_IO1 to EXT_IO6.
const EXPOSED: [u8; 6] = [10, 11, 12, 13, 14, 21];

/// LCD interface, table 5: data 9, 46, 3, 8, 18, 17, 16, 15, write clock 47,
/// command or data 0 (also the BOOT button), reset 4 (also the touch reset),
/// backlight 45, frame sync 48.
const LCD: &[u8] = &[0, 3, 4, 8, 9, 15, 16, 17, 18, 45, 46, 47, 48];

/// Touch panel, table 5: interrupt 7, I2C data 6, I2C clock 5.
const TOUCH: &[u8] = &[5, 6, 7];

/// microSD slot, table 4: chip select 41, MOSI 40, clock 39, MISO 38. The
/// firmware reads the card in native SD mode, so these are data 3 (held
/// high), command, clock and data 0. GPIO 38 is also the RS-485 B line, see
/// the README.
const SD_CARD: &[u8] = &[38, 39, 40, 41];

/// RS-485 transceiver, table 8: receive 1, direction 2, transmit 42.
const RS485: &[u8] = &[1, 2, 42];

/// Audio amplifier, table 7: LRCK 35, BCLK 36, DOUT 37.
const AUDIO: &[u8] = &[35, 36, 37];

/// Debug connector UART0, table 1: transmit 43, receive 44.
const DEBUG_UART: &[u8] = &[43, 44];

/// USB D- and D+, wired to the USB Serial/JTAG console.
const USB: &[u8] = &[19, 20];

/// Quad SPI flash and PSRAM of the module.
const MODULE_MEMORY: &[u8] = &[26, 27, 28, 29, 30, 31, 32];

/// Every pin that a function of the board uses. None of them is on the
/// Extended IO connector.
const USED_BY_BOARD: u64 = pins_mask(LCD)
    | pins_mask(TOUCH)
    | pins_mask(SD_CARD)
    | pins_mask(RS485)
    | pins_mask(AUDIO)
    | pins_mask(DEBUG_UART)
    | pins_mask(USB)
    | pins_mask(MODULE_MEMORY);

const _: () = assert!(
    pins_mask(&EXPOSED) & USED_BY_BOARD == 0,
    "An exposed GPIO pin is also used by the board"
);

// SAFETY: the withheld mask is every pin except those of `EXPOSED`, and
// nothing else of the firmware uses a pin of `EXPOSED`: the display, touch
// panel, backlight and console take the pins of `USED_BY_BOARD`. The other
// pins of the chip are withheld.
static CONTROLLER: Gpio = unsafe { Gpio::new(LINES, !pins_mask(&EXPOSED)) };

/// The controller of the Extended IO pins.
pub(super) fn controller() -> &'static Gpio {
    &CONTROLLER
}
