//! The classic WT32-SC01 display on HSPI (SPI2).

use alloc::{format, string::String};
use embedded_hal_bus::spi::ExclusiveDevice;
use esp_hal::{
    delay::Delay,
    dma_rx_buffer, dma_tx_buffer,
    gpio::{Level, Output, OutputConfig},
    peripherals::{
        DMA_SPI2, GPIO13, GPIO14, GPIO15, GPIO18, GPIO19, GPIO21, GPIO22, GPIO23, I2C0, SPI2,
    },
    spi::master::{Config, Spi},
    time::Rate,
};
use firmware::{Display, panel};
use mipidsi::{interface::SpiInterface, options::ColorInversion};
use static_cell::StaticCell;

const DRAW_BUFFER_ROWS: usize = 10;
// The 80 MHz APB clock divided by six meets the 66 ns minimum write cycle.
const BUS_FREQUENCY_KILOHERTZ: u32 = 13_333;
static PIXEL_STAGING: StaticCell<[u8; 64]> = StaticCell::new();

pub(super) struct Pins {
    pub spi: SPI2<'static>,
    pub dma: DMA_SPI2<'static>,
    pub data: GPIO13<'static>,
    pub clock: GPIO14<'static>,
    pub select: GPIO15<'static>,
    pub command_data: GPIO21<'static>,
    pub reset: GPIO22<'static>,
    pub backlight: GPIO23<'static>,
    pub i2c: I2C0<'static>,
    pub touch_data: GPIO18<'static>,
    pub touch_clock: GPIO19<'static>,
}

pub(super) fn start(pins: Pins) -> Result<(Display, (u8, u8)), String> {
    let mut backlight = Output::new(pins.backlight, Level::Low, OutputConfig::default());
    let spi = Spi::new(
        pins.spi,
        Config::default().with_frequency(Rate::from_khz(BUS_FREQUENCY_KILOHERTZ)),
    )
    .map_err(|error| format!("cannot configure HSPI: {error:?}"))?
    .with_mosi(pins.data)
    .with_sck(pins.clock)
    .with_dma(pins.dma);

    // esp-hal sends internal slices directly. The transmit staging buffer
    // covers other inputs, such as command parameters in flash.
    let receive = dma_rx_buffer!(4)
        .map_err(|error| format!("cannot configure HSPI receive DMA: {error:?}"))?;
    let transmit = dma_tx_buffer!(256)
        .map_err(|error| format!("cannot configure HSPI transmit DMA: {error:?}"))?;
    let spi = spi.with_buffers(receive, transmit);
    let select = Output::new(pins.select, Level::High, OutputConfig::default());
    let device = ExclusiveDevice::new(spi, select, Delay::new()).unwrap();
    let command_data = Output::new(pins.command_data, Level::High, OutputConfig::default());
    let reset = Output::new(pins.reset, Level::High, OutputConfig::default());

    let result = panel::start(
        SpiInterface::new(device, command_data, PIXEL_STAGING.init([0; 64])),
        reset,
        pins.i2c,
        pins.touch_data,
        pins.touch_clock,
        ColorInversion::Normal,
        DRAW_BUFFER_ROWS,
    )?;
    backlight.set_high();
    Ok(result)
}
