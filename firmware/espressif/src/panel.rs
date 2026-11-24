//! Shared ST7796 panel initialization and FT6336U touch setup.

use crate::{AsynchronousFlush, Display, graphics::TouchInputDevice};
use alloc::{boxed::Box, format, string::String};
use core::fmt::Debug;
use drivers_shared::devices::{
    display::{
        AsynchronousDisplayBus, AsynchronousScreenDevice, MipidsiScreenDevice, SerialInterface,
    },
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
    dcs::SetAddressMode,
    interface::Interface,
    models::{Model, ModelInitError, ST7796},
    options::{ColorInversion, ColorOrder, ModelOptions, Orientation, Rotation},
};
use xila::graphics::{Area, Point};

pub const WIDTH: u16 = 480;
pub const HEIGHT: u16 = 320;

const TOUCH_START_MILLISECONDS: u32 = 200;
// Bound a stalled I²C transaction so touch polling cannot hang the graphics loop.
const TOUCH_TRANSACTION_MILLISECONDS: u64 = 5;
type Touch = TouchInputDevice<Ft6336<I2c<'static, Blocking>>>;

/// Registers written to the controller before the generic initialisation,
/// when a board asks for more than the controller's reset defaults.
///
/// The `mipidsi` ST7796 model only sets sleep, orientation, inversion, pixel
/// format and display on. The reset values of the gamma curves, the power and
/// common-electrode levels and the source timing depend on the controller and
/// panel variant, and open projects for these boards write them explicitly
/// (LovyanGFX's `Panel_ST7796`, which Wireless-Tag's own sample code uses).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Initialisation {
    /// Only the generic sequence: the controller's reset registers apply.
    ControllerDefaults,
    /// Gamma curves and one-dot inversion, without power or timing registers.
    Gamma,
    /// The complete LovyanGFX sequence: one-dot inversion, display function
    /// control, source and gate timing, power levels, common electrode level
    /// and gamma curves.
    Vendor,
}

/// Unlocks the extended command set (`0xF0`, parts one and two).
const UNLOCK_EXTENDED_COMMANDS: [(u8, &[u8]); 2] = [(0xF0, &[0xC3]), (0xF0, &[0x96])];
/// Locks the extended command set again.
const LOCK_EXTENDED_COMMANDS: [(u8, &[u8]); 2] = [(0xF0, &[0x3C]), (0xF0, &[0x69])];
/// Display inversion control (`0xB4`): one-dot inversion.
const ONE_DOT_INVERSION: (u8, &[u8]) = (0xB4, &[0x01]);
/// Positive and negative gamma correction (`0xE0`, `0xE1`).
const GAMMA_CURVES: [(u8, &[u8]); 2] = [
    (
        0xE0,
        &[
            0xF0, 0x09, 0x0B, 0x06, 0x04, 0x15, 0x2F, 0x54, 0x42, 0x3C, 0x17, 0x14, 0x18, 0x1B,
        ],
    ),
    (
        0xE1,
        &[
            0xE0, 0x09, 0x0B, 0x06, 0x04, 0x03, 0x2B, 0x43, 0x42, 0x3B, 0x16, 0x14, 0x17, 0x1B,
        ],
    ),
];
/// Display function control (`0xB6`), display output control adjust (`0xE8`),
/// power control two and three (`0xC1`, `0xC2`) and VCOM control (`0xC5`).
const TIMING_AND_POWER: [(u8, &[u8]); 5] = [
    (0xB6, &[0x80, 0x22, 0x3B]),
    (0xE8, &[0x40, 0x8A, 0x00, 0x00, 0x29, 0x19, 0xA5, 0x33]),
    (0xC1, &[0x06]),
    (0xC2, &[0xA7]),
    (0xC5, &[0x18]),
];

/// The `mipidsi` ST7796 model with optional register tuning before it.
struct TunedST7796(Initialisation);

impl TunedST7796 {
    fn write<PanelInterface: Interface>(
        interface: &mut PanelInterface,
        commands: &[(u8, &[u8])],
    ) -> Result<(), PanelInterface::Error> {
        commands
            .iter()
            .try_for_each(|(command, arguments)| interface.send_command(*command, arguments))
    }
}

impl Model for TunedST7796 {
    type ColorFormat = <ST7796 as Model>::ColorFormat;
    const FRAMEBUFFER_SIZE: (u16, u16) = <ST7796 as Model>::FRAMEBUFFER_SIZE;

    fn init<Delayer, PanelInterface>(
        &mut self,
        interface: &mut PanelInterface,
        delay: &mut Delayer,
        options: &ModelOptions,
    ) -> Result<SetAddressMode, ModelInitError<PanelInterface::Error>>
    where
        Delayer: embedded_hal::delay::DelayNs,
        PanelInterface: Interface,
    {
        // The generic sequence waits 150 ms after the reset pulse and then
        // wakes the controller; these registers are written before that, as
        // LovyanGFX does.
        if self.0 != Initialisation::ControllerDefaults {
            Self::write(interface, &UNLOCK_EXTENDED_COMMANDS)
                .and_then(|()| Self::write(interface, &[ONE_DOT_INVERSION]))
                .map_err(ModelInitError::Interface)?;
            if self.0 == Initialisation::Vendor {
                Self::write(interface, &TIMING_AND_POWER).map_err(ModelInitError::Interface)?;
            }
            Self::write(interface, &GAMMA_CURVES)
                .and_then(|()| Self::write(interface, &LOCK_EXTENDED_COMMANDS))
                .map_err(ModelInitError::Interface)?;
            delay.delay_ms(120);
        }
        ST7796.init(interface, delay, options)
    }
}

/// Four-byte-aligned row buffer so clearing the panel at boot can use DMA
/// directly on controllers that require word-aligned addresses.
#[repr(C, align(4))]
struct AlignedBlackRow([u8; WIDTH as usize * 2]);

impl<Bus, ResetPin> AsynchronousFlush for AsynchronousScreenDevice<Bus, ResetPin>
where
    Bus: AsynchronousDisplayBus + Send,
    ResetPin: Send + Sync,
{
    unsafe fn start_flush(&self, area: Area, data: &[u8]) -> bool {
        // SAFETY: `AsynchronousFlush::start_flush` carries the same lifetime contract
        // as `AsynchronousScreenDevice::start_flush`.
        unsafe { AsynchronousScreenDevice::start_flush(self, area, data) }
    }

    fn wait_for_flush(&self) {
        AsynchronousScreenDevice::wait_for_flush(self);
    }
}

fn initialize_panel<PanelInterface>(
    interface: PanelInterface,
    reset: Output<'static>,
    inversion: ColorInversion,
    initialisation: Initialisation,
) -> Result<mipidsi::Display<PanelInterface, TunedST7796, Output<'static>>, String>
where
    PanelInterface: Interface<Word = u8>,
    PanelInterface::Error: Debug,
{
    let mut display = Builder::new(TunedST7796(initialisation), interface)
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

    let black_row = AlignedBlackRow([0u8; WIDTH as usize * 2]);
    for row in 0..HEIGHT {
        display
            .set_pixels_raw(0, row, WIDTH - 1, row, &black_row.0)
            .map_err(|error| format!("cannot clear the panel: {error:?}"))?;
    }
    Ok(display)
}

fn initialize_touch(
    i2c: I2C0<'static>,
    touch_data: impl PeripheralInput<'static> + PeripheralOutput<'static>,
    touch_clock: impl PeripheralInput<'static> + PeripheralOutput<'static>,
) -> Result<(&'static Touch, (u8, u8)), String> {
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
    Ok((touch, (chip, vendor)))
}

/// Starts the panel and probes the polled touch controller for a board with
/// the WT32-SC01 landscape wiring.
pub fn start<PanelInterface>(
    interface: PanelInterface,
    reset: Output<'static>,
    i2c: I2C0<'static>,
    touch_data: impl PeripheralInput<'static> + PeripheralOutput<'static>,
    touch_clock: impl PeripheralInput<'static> + PeripheralOutput<'static>,
    inversion: ColorInversion,
    initialisation: Initialisation,
    draw_buffer_rows: usize,
) -> Result<(Display, (u8, u8)), String>
where
    PanelInterface: Interface<Word = u8> + Send + 'static,
    PanelInterface::Error: Debug,
{
    let display = initialize_panel(interface, reset, inversion, initialisation)?;
    let (touch, identifiers) = initialize_touch(i2c, touch_data, touch_clock)?;

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
            asynchronous_flush: None,
            refresh_period_milliseconds: None,
            align_area_width_even: false,
        },
        identifiers,
    ))
}

/// Starts the panel with a double-buffered asynchronous DMA bus and probes the
/// polled touch controller for a board with the WT32-SC01 landscape wiring.
#[expect(clippy::too_many_arguments)]
pub fn start_asynchronous<Bus>(
    bus: Bus,
    reset: Output<'static>,
    i2c: I2C0<'static>,
    touch_data: impl PeripheralInput<'static> + PeripheralOutput<'static>,
    touch_clock: impl PeripheralInput<'static> + PeripheralOutput<'static>,
    inversion: ColorInversion,
    initialisation: Initialisation,
    draw_buffer_rows: usize,
    refresh_period_milliseconds: u32,
) -> Result<(Display, (u8, u8)), String>
where
    Bus: AsynchronousDisplayBus + Send + 'static,
{
    let display = initialize_panel(SerialInterface::new(bus), reset, inversion, initialisation)?;
    let (interface, _model, reset) = display.release();
    let bus = interface.release();
    let (touch, identifiers) = initialize_touch(i2c, touch_data, touch_clock)?;

    let screen: &'static AsynchronousScreenDevice<Bus, Option<Output<'static>>> =
        Box::leak(Box::new(AsynchronousScreenDevice::new(
            bus,
            reset,
            Point::new(WIDTH as i16, HEIGHT as i16),
        )));
    Ok((
        Display {
            screen,
            pointer: Some(touch),
            draw_buffer_pixels: WIDTH as usize * draw_buffer_rows,
            double_buffered: true,
            byte_swapped_rgb565: true,
            internal_draw_buffers: true,
            asynchronous_flush: Some(screen),
            refresh_period_milliseconds: Some(refresh_period_milliseconds),
            align_area_width_even: true,
        },
        identifiers,
    ))
}
