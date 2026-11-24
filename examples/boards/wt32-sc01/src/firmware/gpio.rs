//! The GPIO devices of the WT32-SC01: `/devices/gpio/pin<number>` for the pins
//! of the two expansion connectors that the board does not use.
//!
//! Source: Wireless-Tag WT32-SC01 datasheet V1.0, page 7, figure 4 "Interface
//! Definition Diagram", the 2 x 20 expansion connectors (items 8 and 9, wired
//! pin to pin). The figure labels these GPIO signals:
//!
//! | Connector signal | GPIO |
//! |---|---|
//! | IO0, IO2, IO4, IO5 | 0, 2, 4, 5 |
//! | IO12, IO13, IO14, IO15 | 12, 13, 14, 15 |
//! | IO21, IO22, IO23 | 21, 22, 23 |
//! | IO25, IO26, IO27 | 25, 26, 27 |
//! | IO32, IO33, IO34, IO35 | 32, 33, 34, 35 |
//! | SENSOR_VP, SENSOR_VN | 36, 39 |
//! | TXD | 1 |
//! | I2C_SDA, I2C_SCL | 18, 19 (the figure gives no GPIO number; see the touch pins in `screen.rs`) |
//!
//! The controller exposes the pins of this list that nothing on the board
//! uses: [`EXPOSED`]. It withholds every other pin, so configuring a device
//! can never reconfigure a pin of the display, the touch panel, the console, the
//! download circuit, the flash or the PSRAM. [`USED_BY_BOARD`] collects those
//! pins and the build fails when one of them is also in [`EXPOSED`].
//!
//! GPIO 2, 5 and 12 are strapping pins: the chip samples them at reset only.
//! GPIO 34, 35, 36 are input-only.

use drivers_espressif::{
    chips::LINES,
    shared::{Gpio, pins_mask},
};

/// Connector pins that no function of the board uses.
const EXPOSED: [u8; 12] = [2, 4, 5, 12, 25, 26, 27, 32, 33, 34, 35, 36];

/// Console: UART0 transmit 1 (connector signal TXD) and receive 3.
const CONSOLE: &[u8] = &[1, 3];

/// Download circuit: the USB-UART bridge drives GPIO 0 and EN to enter the
/// boot loader.
const DOWNLOAD: &[u8] = &[0];

/// Display on HSPI (`screen.rs`): data 13, clock 14, chip select 15, command
/// or data 21, reset 22, backlight 23.
const DISPLAY: &[u8] = &[13, 14, 15, 21, 22, 23];

/// Touch panel: I2C data 18, clock 19, and interrupt 39 (not read, but the
/// touch controller drives it).
const TOUCH: &[u8] = &[18, 19, 39];

/// Flash of the module (GPIO 6 to 11) and its PSRAM (GPIO 16 and 17).
const MODULE_MEMORY: &[u8] = &[6, 7, 8, 9, 10, 11, 16, 17];

/// Every pin that a function of the board uses. Some of them are on the
/// connectors too, as the figure shows.
const USED_BY_BOARD: u64 = pins_mask(CONSOLE)
    | pins_mask(DOWNLOAD)
    | pins_mask(DISPLAY)
    | pins_mask(TOUCH)
    | pins_mask(MODULE_MEMORY);

const _: () = assert!(
    pins_mask(&EXPOSED) & USED_BY_BOARD == 0,
    "An exposed GPIO pin is also used by the board"
);

// SAFETY: the withheld mask is every pin except those of `EXPOSED`, and
// nothing else of the firmware uses a pin of `EXPOSED`: the display, touch
// panel and console take the pins of `USED_BY_BOARD`. The other pins of the
// chip are withheld.
static CONTROLLER: Gpio = unsafe { Gpio::new(LINES, !pins_mask(&EXPOSED)) };

/// The controller of the expansion connector pins.
pub(super) fn controller() -> &'static Gpio {
    &CONTROLLER
}
