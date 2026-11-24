//! The 3.5-inch 480 × 320 ST7796UI panel on an 8-bit 8080 bus, its backlight
//! and the FT6336U capacitive touch controller.
//!
//! Pins, from the Wireless-Tag WT32-SC01 Plus datasheet (V1.3, table 5, "LCD
//! Interface"); RIOT's `esp32s3-wt32-sc01-plus` board, espp's
//! `smartpanlee-sc01-plus` component and esp-hal's `lcd-i8080` example agree:
//!
//! | Signal | GPIO |
//! |---|---|
//! | LCD_DB0 to LCD_DB7 | 9, 46, 3, 8, 18, 17, 16, 15 |
//! | LCD_WR (write clock) | 47 |
//! | LCD_RS (command or data) | 0, also the BOOT button |
//! | LCD_RESET, also TP_RST | 4 |
//! | BL_PWM (backlight, active high) | 45 |
//! | TP_SDA, TP_SCL | 6, 5 |
//! | TP_INT | 7 (not used: the controller is polled) |
//!
//! The panel has no chip select or read strobe. Panel settings, as in RIOT's
//! board, LovyanGFX configurations for this board and espp: colour
//! inversion on, BGR order, landscape with rows and columns swapped. The
//! touch panel reports portrait coordinates; RIOT maps them to the landscape
//! screen by swapping the axes and mirroring Y.

use alloc::{format, string::String};
use core::{
    cell::RefCell,
    ptr::addr_of_mut,
    sync::atomic::{AtomicU16, Ordering},
};

use critical_section::Mutex;

use drivers_espressif::lcd::I8080Bus;
use drivers_shared::devices::display::ParallelInterface;
use esp_hal::{
    dma::DmaDescriptor,
    gpio::{Level, Output, OutputConfig},
    lcd_cam::{
        LcdCam,
        lcd::i8080::{Config as I8080Configuration, I8080},
    },
    peripherals::{
        DMA_CH0, GPIO0, GPIO3, GPIO4, GPIO5, GPIO6, GPIO8, GPIO9, GPIO15, GPIO16, GPIO17, GPIO18,
        GPIO45, GPIO46, GPIO47, I2C0, LCD_CAM,
    },
    time::Rate,
};
use firmware::{
    Display,
    panel::{self, HEIGHT, Initialisation, WIDTH},
};
use mipidsi::options::ColorInversion;
use xila::log;

/// The panel registers written at start-up.
const INITIALISATION: Initialisation = if cfg!(feature = "panel-vendor-registers") {
    Initialisation::Vendor
} else if cfg!(feature = "panel-gamma") {
    Initialisation::Gamma
} else {
    Initialisation::ControllerDefaults
};

/// LVGL renders 40 rows at a time: 38,400 bytes in internal SRAM.
const DRAW_BUFFER_ROWS: usize = 40;

/// The 8080 write clock: the rate esp-hal's example for this board uses.
const BUS_FREQUENCY_MEGAHERTZ: u32 = 20;

/// DMA descriptors of the bus, 4092 bytes each: more than one draw buffer.
const DESCRIPTOR_COUNT: usize = 16;
static mut DESCRIPTORS: [DmaDescriptor; DESCRIPTOR_COUNT] =
    [DmaDescriptor::EMPTY; DESCRIPTOR_COUNT];

/// The touch controller's chip and vendor identifiers, read at start-up and
/// logged once the logger runs.
static TOUCH_IDENTIFIERS: AtomicU16 = AtomicU16::new(0);

/// Why the screen could not start, logged once the logger runs.
static FAILURE: Mutex<RefCell<Option<String>>> = Mutex::new(RefCell::new(None));

/// The peripherals of the screen and the touch panel.
pub(super) struct Pins {
    pub lcd_cam: LCD_CAM<'static>,
    pub dma: DMA_CH0<'static>,
    pub data: (
        GPIO9<'static>,
        GPIO46<'static>,
        GPIO3<'static>,
        GPIO8<'static>,
        GPIO18<'static>,
        GPIO17<'static>,
        GPIO16<'static>,
        GPIO15<'static>,
    ),
    pub write_clock: GPIO47<'static>,
    pub command_data: GPIO0<'static>,
    pub reset: GPIO4<'static>,
    pub backlight: GPIO45<'static>,
    pub i2c: I2C0<'static>,
    pub touch_data: GPIO6<'static>,
    pub touch_clock: GPIO5<'static>,
}

/// Initialises the panel, clears it, turns the backlight on and starts the
/// touch controller. On failure, the board runs without its screen:
/// [`log_report`] logs why.
pub(super) fn start(pins: Pins) -> Option<Display> {
    match try_start(pins) {
        Ok(display) => Some(display),
        Err(failure) => {
            critical_section::with(|section| {
                FAILURE.borrow(section).replace(Some(failure));
            });
            None
        }
    }
}

/// Starts the panel and the touch controller, then turns the backlight on.
/// The graphical login needs touch, so a touch controller that does not answer
/// is a failure too: the backlight stays off and the board runs headless.
fn try_start(pins: Pins) -> Result<Display, String> {
    // Dark until everything works and the panel holds a black frame.
    let mut backlight = Output::new(pins.backlight, Level::Low, OutputConfig::default());
    let reset = Output::new(pins.reset, Level::High, OutputConfig::default());

    let lcd_cam = LcdCam::new(pins.lcd_cam);
    let (data_0, data_1, data_2, data_3, data_4, data_5, data_6, data_7) = pins.data;
    let i8080 = I8080::new(
        lcd_cam.lcd,
        pins.dma,
        I8080Configuration::default().with_frequency(Rate::from_mhz(BUS_FREQUENCY_MEGAHERTZ)),
    )
    .map_err(|error| format!("cannot configure the LCD bus: {error:?}"))?
    .with_dc(pins.command_data)
    .with_wrx(pins.write_clock)
    .with_data0(data_0)
    .with_data1(data_1)
    .with_data2(data_2)
    .with_data3(data_3)
    .with_data4(data_4)
    .with_data5(data_5)
    .with_data6(data_6)
    .with_data7(data_7);
    // SAFETY: this runs once, at start-up, and is the only user of the
    // descriptors.
    let descriptors = unsafe { &mut *addr_of_mut!(DESCRIPTORS) };
    let bus = I8080Bus::new(i8080, descriptors)
        .map_err(|error| format!("cannot use the LCD bus: {error:?}"))?;

    let (display, (chip, vendor)) = panel::start(
        ParallelInterface::new(bus),
        reset,
        pins.i2c,
        pins.touch_data,
        pins.touch_clock,
        ColorInversion::Inverted,
        INITIALISATION,
        DRAW_BUFFER_ROWS,
    )?;
    TOUCH_IDENTIFIERS.store(u16::from(chip) << 8 | u16::from(vendor), Ordering::Relaxed);
    // esp-hal's `Output` has no `Drop`: the pin keeps driving the backlight
    // on after `backlight` goes out of scope.
    backlight.set_high();

    Ok(display)
}

/// Logs what start-up found, once the logger runs.
pub(super) fn log_report() {
    if let Some(failure) = critical_section::with(|section| FAILURE.borrow(section).take()) {
        log::error!("Xila: the screen is off, {failure}; only the console session starts");
        return;
    }
    log::information!(
        "Xila: ST7796 panel {WIDTH}x{HEIGHT} on the 8080 bus at {BUS_FREQUENCY_MEGAHERTZ} MHz, backlight on"
    );
    let identifiers = TOUCH_IDENTIFIERS.load(Ordering::Relaxed);
    log::information!(
        "Xila: FT6336U touch controller (chip {:#04x}, vendor {:#04x})",
        identifiers >> 8,
        identifiers & 0xFF
    );
}
