//! The classic WT32-SC01 display on HSPI (SPI2) with asynchronous DMA.

use alloc::{format, string::String};
use core::ptr::addr_of_mut;
use drivers_espressif::spi_display::{
    DESCRIPTOR_CHUNK_SIZE, MAXIMUM_DMA_TRANSFER_SIZE, MINIMUM_DMA_TRANSFER_SIZE, SpiDisplayBus,
};
use esp_hal::{
    dma::DmaDescriptor,
    gpio::{Level, Output, OutputConfig},
    peripherals::{
        DMA_SPI2, GPIO13, GPIO14, GPIO15, GPIO18, GPIO19, GPIO21, GPIO22, GPIO23, I2C0, LEDC, SPI2,
    },
    spi::master::{Config as SpiConfiguration, Spi},
    time::Rate,
};
use firmware::{
    Display, backlight,
    panel::{self, WIDTH},
};
use mipidsi::options::ColorInversion;

/// Two 20-row RGB565 draw buffers in internal SRAM (19,200 bytes each,
/// 38,400 bytes total): 16 exact strips across the 320-row panel, each fitting
/// in a single ESP32 SPI DMA transfer (at most 32,736 bytes).
const DRAW_BUFFER_ROWS: usize = 20;
const DRAW_BUFFER_BYTES: usize = WIDTH as usize * DRAW_BUFFER_ROWS * 2;

/// DMA descriptors for one draw buffer (4092 bytes per descriptor).
const DESCRIPTOR_COUNT: usize = DRAW_BUFFER_BYTES.div_ceil(DESCRIPTOR_CHUNK_SIZE);

const _: () = {
    assert!(DRAW_BUFFER_BYTES <= MAXIMUM_DMA_TRANSFER_SIZE);
    assert!(DESCRIPTOR_COUNT * DESCRIPTOR_CHUNK_SIZE >= DRAW_BUFFER_BYTES);
};

static mut DESCRIPTORS: [DmaDescriptor; DESCRIPTOR_COUNT] =
    [DmaDescriptor::EMPTY; DESCRIPTOR_COUNT];

// The 80 MHz APB clock divided by two: a 25 ns write cycle, faster than the
// ST7796S specification's 66 ns minimum, as widely used with this panel.
// Validated on a WT32-SC01 (clean image and touch). The fastest in-specification
// setting is 13_334 (divider six, 75 ns); esp-hal rounds the divider up, so
// 13_333 would give divider seven (11.4 MHz).
const BUS_FREQUENCY_KILOHERTZ: u32 = 40_000;

/// LVGL display refresh and touch polling period (~60 Hz).
const REFRESH_PERIOD_MILLISECONDS: u32 = 16;

pub(super) struct Pins {
    pub spi: SPI2<'static>,
    pub dma: DMA_SPI2<'static>,
    pub data: GPIO13<'static>,
    pub clock: GPIO14<'static>,
    pub select: GPIO15<'static>,
    pub command_data: GPIO21<'static>,
    pub reset: GPIO22<'static>,
    /// The backlight, active high, dimmed by LEDC PWM.
    pub backlight: GPIO23<'static>,
    pub ledc: LEDC<'static>,
    pub i2c: I2C0<'static>,
    pub touch_data: GPIO18<'static>,
    pub touch_clock: GPIO19<'static>,
}

pub(super) fn start(pins: Pins) -> Result<(Display, (u8, u8)), String> {
    // Dark until the panel holds a black frame.
    let backlight = backlight::start(pins.ledc, pins.backlight)?;
    let spi = Spi::new(
        pins.spi,
        SpiConfiguration::default()
            .with_frequency(Rate::from_khz(BUS_FREQUENCY_KILOHERTZ))
            .with_min_async_transfer_size(MINIMUM_DMA_TRANSFER_SIZE),
    )
    .map_err(|error| format!("cannot configure HSPI: {error:?}"))?
    .with_mosi(pins.data)
    .with_sck(pins.clock)
    .with_dma(pins.dma);

    let select = Output::new(pins.select, Level::High, OutputConfig::default());
    let command_data = Output::new(pins.command_data, Level::High, OutputConfig::default());
    let reset = Output::new(pins.reset, Level::High, OutputConfig::default());

    // SAFETY: `screen::start` runs once at boot and is the sole owner of
    // `DESCRIPTORS` for the rest of the firmware's lifetime.
    let descriptors = unsafe { &mut *addr_of_mut!(DESCRIPTORS) };
    let bus = SpiDisplayBus::new(spi, select, command_data, descriptors)
        .map_err(|error| format!("cannot configure HSPI display bus: {error:?}"))?;

    let (mut display, identifiers) = panel::start_asynchronous(
        bus,
        reset,
        pins.i2c,
        pins.touch_data,
        pins.touch_clock,
        ColorInversion::Normal,
        DRAW_BUFFER_ROWS,
        REFRESH_PERIOD_MILLISECONDS,
    )?;
    // The 40 MHz SPI bus needs at least 61 ms to send a whole frame, so an
    // animation over most of the screen shows three or four of its steps.
    display.light_rendering = true;
    // Full brightness until the saved appearance sets the user's level.
    backlight::attach(&mut display, backlight)?;
    Ok((display, identifiers))
}
