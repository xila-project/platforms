//! Shared ST7796 panel initialization and FT6336U touch setup.

use crate::{Display, graphics::TouchInputDevice};
use alloc::{boxed::Box, format, string::String};
use core::fmt::Debug;
use drivers_shared::devices::{
    display::MipidsiScreenDevice,
    touch::{FT6336_ADDRESS, Ft6336, TouchTransform},
};
use esp_hal::{
    Blocking,
    delay::Delay,
    gpio::{
        Output,
        interconnect::{PeripheralInput, PeripheralOutput},
    },
    i2c::master::{BusTimeout, Config as I2cConfiguration, I2c, SoftwareTimeout},
    peripherals::I2C0,
    time::{Duration, Rate},
};
use mipidsi::{
    Builder,
    interface::Interface,
    models::ST7796,
    options::{ColorInversion, ColorOrder, Orientation, Rotation},
};
use xila::graphics::Point;

pub const WIDTH: u16 = 480;
pub const HEIGHT: u16 = 320;

const TOUCH_START_MILLISECONDS: u32 = 200;
// Polling holds a critical section. Bound a stalled I²C transaction.
const TOUCH_TRANSACTION_MILLISECONDS: u64 = 5;
type Touch = TouchInputDevice<Ft6336<I2c<'static, Blocking>>>;

/// Starts the panel and probes the polled touch controller for a board with
/// the WT32-SC01 landscape wiring.
pub fn start<PanelInterface>(
    interface: PanelInterface,
    reset: Output<'static>,
    i2c: I2C0<'static>,
    touch_data: impl PeripheralInput<'static> + PeripheralOutput<'static>,
    touch_clock: impl PeripheralInput<'static> + PeripheralOutput<'static>,
    inversion: ColorInversion,
    draw_buffer_rows: usize,
) -> Result<(Display, (u8, u8)), String>
where
    PanelInterface: Interface<Word = u8> + Send + 'static,
    PanelInterface::Error: Debug,
{
    let mut display = Builder::new(ST7796, interface)
        .reset_pin(reset)
        .display_size(HEIGHT, WIDTH)
        .orientation(Orientation {
            rotation: Rotation::Deg90,
            mirrored: true,
        })
        .color_order(ColorOrder::Bgr)
        .invert_colors(inversion)
        .init(&mut Delay::new())
        .map_err(|error| format!("cannot initialise the ST7796 panel: {error:?}"))?;

    let black_row = [0u8; WIDTH as usize * 2];
    for row in 0..HEIGHT {
        display
            .set_pixels_raw(0, row, WIDTH - 1, row, &black_row)
            .map_err(|error| format!("cannot clear the panel: {error:?}"))?;
    }

    let i2c = I2c::new(
        i2c,
        I2cConfiguration::default()
            .with_frequency(Rate::from_khz(400))
            .with_timeout(BusTimeout::Maximum)
            .with_software_timeout(SoftwareTimeout::Transaction(Duration::from_millis(
                TOUCH_TRANSACTION_MILLISECONDS,
            ))),
    )
    .map_err(|error| format!("cannot configure the touch I²C bus: {error:?}"))?
    .with_sda(touch_data)
    .with_scl(touch_clock);
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
    let (chip, vendor) = controller.identifiers().map_err(|error| {
        format!("the FT6336U touch controller does not answer at {FT6336_ADDRESS:#04x} ({error:?})")
    })?;
    let touch: &'static Touch = Box::leak(Box::new(TouchInputDevice::new(controller)));

    let screen = Box::leak(Box::new(MipidsiScreenDevice::new(
        display,
        Point::new(WIDTH as i16, HEIGHT as i16),
    )));
    Ok((
        Display {
            screen,
            pointer: Some(touch),
            draw_buffer_pixels: WIDTH as usize * draw_buffer_rows,
            double_buffered: false,
            byte_swapped_rgb565: true,
            internal_draw_buffers: true,
        },
        (chip, vendor),
    ))
}
