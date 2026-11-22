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

use alloc::boxed::Box;
use core::{
    ptr::addr_of_mut,
    sync::atomic::{AtomicU16, Ordering},
};

use drivers_espressif::lcd::I8080Bus;
use drivers_shared::devices::{
    display::{MipidsiScreenDevice, ParallelInterface},
    touch::{FT6336_ADDRESS, Ft6336, TouchTransform},
};
use esp_hal::{
    Blocking,
    delay::Delay,
    dma::DmaDescriptor,
    gpio::{Level, Output, OutputConfig},
    i2c::master::{Config as I2cConfiguration, I2c},
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
use firmware::{Display, graphics::TouchInputDevice};
use mipidsi::{
    Builder,
    models::ST7796,
    options::{ColorInversion, ColorOrder, Orientation, Rotation},
};
use xila::{graphics::Point, log};

/// Screen size in landscape.
const WIDTH: u16 = 480;
const HEIGHT: u16 = 320;

/// LVGL renders 40 rows at a time: 38,400 bytes in internal SRAM.
const DRAW_BUFFER_ROWS: usize = 40;

/// The 8080 write clock: the rate esp-hal's example for this board uses.
const BUS_FREQUENCY_MEGAHERTZ: u32 = 20;

/// DMA descriptors of the bus, 4092 bytes each: more than one draw buffer.
const DESCRIPTOR_COUNT: usize = 16;
static mut DESCRIPTORS: [DmaDescriptor; DESCRIPTOR_COUNT] =
    [DmaDescriptor::EMPTY; DESCRIPTOR_COUNT];

/// How long the touch controller needs after the shared reset before it
/// answers on I²C; the panel's initialisation already took most of it.
const TOUCH_START_MILLISECONDS: u32 = 200;

/// The touch controller's chip and vendor identifiers, read at start-up and
/// logged once the logger runs; zero when it did not answer.
static TOUCH_IDENTIFIERS: AtomicU16 = AtomicU16::new(0);

type Screen = MipidsiScreenDevice<ParallelInterface<I8080Bus>, ST7796, Output<'static>>;
type Touch = TouchInputDevice<Ft6336<I2c<'static, Blocking>>>;

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
/// touch controller.
pub(super) fn start(pins: Pins) -> Display {
    // Dark until the panel holds a black frame.
    let mut backlight = Output::new(pins.backlight, Level::Low, OutputConfig::default());
    let reset = Output::new(pins.reset, Level::High, OutputConfig::default());

    let lcd_cam = LcdCam::new(pins.lcd_cam);
    let (d0, d1, d2, d3, d4, d5, d6, d7) = pins.data;
    let i8080 = I8080::new(
        lcd_cam.lcd,
        pins.dma,
        I8080Configuration::default().with_frequency(Rate::from_mhz(BUS_FREQUENCY_MEGAHERTZ)),
    )
    .expect("Failed to configure the LCD bus")
    .with_dc(pins.command_data)
    .with_wrx(pins.write_clock)
    .with_data0(d0)
    .with_data1(d1)
    .with_data2(d2)
    .with_data3(d3)
    .with_data4(d4)
    .with_data5(d5)
    .with_data6(d6)
    .with_data7(d7);
    // SAFETY: this runs once, at start-up, and is the only user of the
    // descriptors, which live in internal SRAM.
    let descriptors = unsafe { &mut *addr_of_mut!(DESCRIPTORS) };
    let bus = I8080Bus::new(i8080, descriptors);

    // The reset also resets the touch controller.
    let mut display = Builder::new(ST7796, ParallelInterface::new(bus))
        .reset_pin(reset)
        .display_size(HEIGHT, WIDTH)
        .orientation(Orientation {
            rotation: Rotation::Deg90,
            mirrored: true,
        })
        .color_order(ColorOrder::Bgr)
        .invert_colors(ColorInversion::Inverted)
        .init(&mut Delay::new())
        .expect("Failed to initialise the ST7796 panel");

    let black_row = [0u8; WIDTH as usize * 2];
    for row in 0..HEIGHT {
        display
            .set_pixels_raw(0, row, WIDTH - 1, row, &black_row)
            .expect("Failed to clear the panel");
    }
    // esp-hal's `Output` has no `Drop`: the pin keeps driving the backlight
    // on after `backlight` goes out of scope.
    backlight.set_high();

    let screen: &'static Screen = Box::leak(Box::new(MipidsiScreenDevice::new(
        display,
        Point::new(WIDTH as i16, HEIGHT as i16),
    )));

    let i2c = I2c::new(
        pins.i2c,
        I2cConfiguration::default().with_frequency(Rate::from_khz(400)),
    )
    .expect("Failed to configure the touch I²C bus")
    .with_sda(pins.touch_data)
    .with_scl(pins.touch_clock);
    let mut controller = Ft6336::new(
        i2c,
        FT6336_ADDRESS,
        TouchTransform {
            width: WIDTH,
            height: HEIGHT,
            swap_axes: true,
            mirror_x: false,
            mirror_y: true,
        },
    );
    Delay::new().delay_millis(TOUCH_START_MILLISECONDS);
    if let Ok((chip, vendor)) = controller.identifiers() {
        TOUCH_IDENTIFIERS.store(u16::from(chip) << 8 | u16::from(vendor), Ordering::Relaxed);
    }
    let touch: &'static Touch = Box::leak(Box::new(TouchInputDevice::new(controller)));

    Display {
        screen,
        pointer: Some(touch),
        draw_buffer_pixels: WIDTH as usize * DRAW_BUFFER_ROWS,
        double_buffered: false,
        byte_swapped_rgb565: true,
        internal_draw_buffers: true,
    }
}

/// Logs what start-up found, once the logger runs.
pub(super) fn log_report() {
    log::information!(
        "Xila: ST7796 panel {WIDTH}x{HEIGHT} on the 8080 bus at {BUS_FREQUENCY_MEGAHERTZ} MHz, backlight on"
    );
    match TOUCH_IDENTIFIERS.load(Ordering::Relaxed) {
        0 => log::warning!("Xila: the FT6336U touch controller does not answer at 0x38"),
        identifiers => log::information!(
            "Xila: FT6336U touch controller (chip {:#04x}, vendor {:#04x})",
            identifiers >> 8,
            identifiers & 0xFF
        ),
    }
}
