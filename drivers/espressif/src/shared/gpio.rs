use core::cell::Cell;

use critical_section::Mutex;
use drivers_peripherals::gpio::mount_gpio;
use esp_hal::gpio::{
    AnyPin, DriveMode as HalDriveMode, Flex, InputConfig as HalInputConfig, Level,
    OutputConfig as HalOutputConfig, Pull as HalPull,
};
use peripherals::gpio::{
    DriveMode, GpioController, InputConfig, LineCapabilities, LineId, OutputConfig, PinState, Pull,
};
use task::TaskIdentifier;
use virtual_file_system::VirtualFileSystem;

use super::{Error, Result};

/// Limitations of a GPIO line, from esp-hal's chip metadata.
///
/// A flagged line works as a GPIO, but something else may need it. The board
/// decides: pass the flags it wants withheld to [`reserved_mask`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LineFlags(u8);

impl LineFlags {
    /// No limitation.
    pub const NONE: Self = Self(0);
    /// Sampled at reset to select the boot mode: an external circuit holding it
    /// at the wrong level changes how the chip boots.
    pub const STRAPPING: Self = Self(1 << 0);
    /// Carries an external JTAG debugger.
    pub const JTAG: Self = Self(1 << 1);
    /// UART0 TX or RX, used by the ROM bootloader and the default console.
    pub const CONSOLE: Self = Self(1 << 2);
    /// Used by PSRAM, or by octal flash/PSRAM, on modules that have them.
    pub const MODULE_MEMORY: Self = Self(1 << 3);

    pub const fn union(self, other: Self) -> Self {
        Self(self.0 | other.0)
    }

    pub const fn intersects(self, other: Self) -> bool {
        self.0 & other.0 != 0
    }
}

/// One physical GPIO line of a chip, its VFS device name and the features it supports.
#[derive(Clone, Copy, Debug)]
pub struct Line {
    pub number: u8,
    pub name: &'static str,
    pub capabilities: LineCapabilities,
    pub flags: LineFlags,
}

/// Capabilities of a line that can be an input or an output, with pulls and open drain.
pub const FULL_CAPABILITIES: LineCapabilities = LineCapabilities {
    input: true,
    output: true,
    pull_up: true,
    pull_down: true,
    open_drain: true,
};

/// Capabilities of a line that can only be an input, without pull resistors.
pub const INPUT_ONLY_CAPABILITIES: LineCapabilities = LineCapabilities {
    input: true,
    output: false,
    pull_up: false,
    pull_down: false,
    open_drain: false,
};

/// Declares a line of `FULL_CAPABILITIES`, mounted at `/devices/gpio/pin<number>`.
#[macro_export]
macro_rules! pin_line {
    ($number:literal) => {
        $crate::pin_line!($number, $crate::shared::LineFlags::NONE)
    };
    ($number:literal, $flags:expr) => {
        $crate::shared::Line {
            number: $number,
            name: concat!("pin", $number),
            capabilities: $crate::shared::FULL_CAPABILITIES,
            flags: $flags,
        }
    };
}

/// Declares a line of `INPUT_ONLY_CAPABILITIES`, mounted at `/devices/gpio/pin<number>`.
#[macro_export]
macro_rules! pin_input_only {
    ($number:literal) => {
        $crate::pin_input_only!($number, $crate::shared::LineFlags::NONE)
    };
    ($number:literal, $flags:expr) => {
        $crate::shared::Line {
            number: $number,
            name: concat!("pin", $number),
            capabilities: $crate::shared::INPUT_ONLY_CAPABILITIES,
            flags: $flags,
        }
    };
}

/// Bit of GPIO `number` in a mask. Panics, or fails to compile in a `const`
/// or `static`, when the number does not fit: wrapping would withhold the wrong pin.
const fn mask_bit(number: u8) -> u64 {
    assert!(number < 64, "GPIO number out of range for a 64-bit mask");
    1 << number
}

/// Mask of the lines of `lines` that have any of `flags`, for [`Gpio::new`].
pub const fn reserved_mask(lines: &[Line], flags: LineFlags) -> u64 {
    let mut mask = 0;
    let mut index = 0;
    while index < lines.len() {
        if lines[index].flags.intersects(flags) {
            mask |= mask_bit(lines[index].number);
        }
        index += 1;
    }
    mask
}

/// Mask of the given GPIO numbers, for [`Gpio::new`].
pub const fn pins_mask(pins: &[u8]) -> u64 {
    let mut mask = 0;
    let mut index = 0;
    while index < pins.len() {
        mask |= mask_bit(pins[index]);
        index += 1;
    }
    mask
}

/// ESP GPIO controller implementing the portable `GpioController` contract.
///
/// `LineId` is the physical GPIO number. Withheld lines do not exist for the
/// controller. A bit per GPIO number records which lines have a live handle,
/// so one pin cannot be requested twice.
pub struct Gpio {
    lines: &'static [Line],
    withheld: u64,
    in_use: Mutex<Cell<u64>>,
}

impl Gpio {
    /// Creates a controller over `lines` without the lines whose bit is set in
    /// `withheld` (see [`reserved_mask`] and [`pins_mask`]).
    ///
    /// # Safety
    ///
    /// Requesting a line reconfigures its pin. Every line of `lines` that is
    /// not withheld must not be used by anything else for the controller's
    /// lifetime: no other esp-hal driver, peripheral signal or flash/PSRAM.
    pub const unsafe fn new(lines: &'static [Line], withheld: u64) -> Self {
        Self {
            lines,
            withheld,
            in_use: Mutex::new(Cell::new(0)),
        }
    }

    /// The lines the controller exposes, in table order.
    pub fn lines(&self) -> impl Iterator<Item = &'static Line> + '_ {
        self.lines.iter().filter(
            |line| matches!(reservation_bit(line.number), Ok(bit) if self.withheld & bit == 0),
        )
    }

    fn line(&self, id: LineId) -> Result<Line> {
        self.lines()
            .copied()
            .find(|line| u16::from(line.number) == id.offset())
            .ok_or(Error::InvalidLine)
    }

    fn reserve(&self, number: u8) -> Result<()> {
        let bit = reservation_bit(number)?;

        critical_section::with(|cs| {
            let in_use = self.in_use.borrow(cs);
            let bits = in_use.get();

            if bits & bit != 0 {
                return Err(Error::AlreadyReserved);
            }

            in_use.set(bits | bit);
            Ok(())
        })
    }

    fn release(&self, number: u8) {
        if let Ok(bit) = reservation_bit(number) {
            critical_section::with(|cs| {
                let in_use = self.in_use.borrow(cs);
                in_use.set(in_use.get() & !bit);
            });
        }
    }
}

fn reservation_bit(number: u8) -> Result<u64> {
    1u64.checked_shl(u32::from(number))
        .ok_or(Error::InvalidLine)
}

fn check_pull(capabilities: &LineCapabilities, pull: Pull) -> Result<()> {
    let supported = match pull {
        Pull::Disabled => true,
        Pull::Up => capabilities.pull_up,
        Pull::Down => capabilities.pull_down,
    };

    if supported {
        Ok(())
    } else {
        Err(Error::Unsupported)
    }
}

fn to_hal_pull(pull: Pull) -> HalPull {
    match pull {
        Pull::Disabled => HalPull::None,
        Pull::Up => HalPull::Up,
        Pull::Down => HalPull::Down,
    }
}

fn to_hal_drive_mode(drive_mode: DriveMode) -> HalDriveMode {
    match drive_mode {
        DriveMode::PushPull => HalDriveMode::PushPull,
        DriveMode::OpenDrain => HalDriveMode::OpenDrain,
    }
}

fn to_hal_level(state: PinState) -> Level {
    if state.is_high() {
        Level::High
    } else {
        Level::Low
    }
}

/// Returns the pin to a high-impedance state so it no longer drives the line
/// or keeps a pull resistor after its handle is gone.
fn detach(pin: &mut Flex<'static>) {
    pin.set_output_enable(false);
    pin.set_input_enable(false);
    pin.apply_input_config(&HalInputConfig::default());
}

impl GpioController for Gpio {
    type Error = Error;

    type Input<'a>
        = GpioInput<'a>
    where
        Self: 'a;

    type Output<'a>
        = GpioOutput<'a>
    where
        Self: 'a;

    fn capabilities(&self, line: LineId) -> Option<LineCapabilities> {
        self.line(line).ok().map(|line| line.capabilities)
    }

    fn request_input(&self, line: LineId, config: InputConfig) -> Result<GpioInput<'_>> {
        let info = self.line(line)?;

        if !info.capabilities.input {
            return Err(Error::Unsupported);
        }

        check_pull(&info.capabilities, config.pull)?;
        self.reserve(info.number)?;

        // SAFETY: the line is not withheld, so `Gpio::new`'s contract leaves the
        // pin to this controller; the reservation above makes this the only
        // live handle, released when the returned `GpioInput` is dropped.
        let mut pin = Flex::new(unsafe { AnyPin::steal(info.number) });
        pin.apply_input_config(&HalInputConfig::default().with_pull(to_hal_pull(config.pull)));
        pin.set_output_enable(false);
        pin.set_input_enable(true);

        Ok(GpioInput {
            gpio: self,
            number: info.number,
            pin,
        })
    }

    fn request_output(&self, line: LineId, config: OutputConfig) -> Result<GpioOutput<'_>> {
        let info = self.line(line)?;

        if !info.capabilities.output {
            return Err(Error::Unsupported);
        }

        check_pull(&info.capabilities, config.pull)?;

        if config.drive_mode == DriveMode::OpenDrain && !info.capabilities.open_drain {
            return Err(Error::Unsupported);
        }

        self.reserve(info.number)?;

        // SAFETY: see `request_input`.
        let mut pin = Flex::new(unsafe { AnyPin::steal(info.number) });
        pin.apply_output_config(
            &HalOutputConfig::default()
                .with_drive_mode(to_hal_drive_mode(config.drive_mode))
                .with_pull(to_hal_pull(config.pull)),
        );
        // Set the level before enabling the driver to avoid a glitch at request time.
        pin.set_level(to_hal_level(config.initial_state));
        pin.set_input_enable(false);
        pin.set_output_enable(true);

        Ok(GpioOutput {
            gpio: self,
            number: info.number,
            pin,
        })
    }
}

/// Input handle returned by `Gpio::request_input`.
pub struct GpioInput<'a> {
    gpio: &'a Gpio,
    number: u8,
    pin: Flex<'static>,
}

impl Drop for GpioInput<'_> {
    fn drop(&mut self) {
        detach(&mut self.pin);
        self.gpio.release(self.number);
    }
}

impl embedded_hal::digital::ErrorType for GpioInput<'_> {
    type Error = Error;
}

impl embedded_hal::digital::InputPin for GpioInput<'_> {
    fn is_high(&mut self) -> Result<bool> {
        Ok(self.pin.is_high())
    }

    fn is_low(&mut self) -> Result<bool> {
        Ok(!self.pin.is_high())
    }
}

/// Output handle returned by `Gpio::request_output`.
pub struct GpioOutput<'a> {
    gpio: &'a Gpio,
    number: u8,
    pin: Flex<'static>,
}

impl Drop for GpioOutput<'_> {
    fn drop(&mut self) {
        detach(&mut self.pin);
        self.gpio.release(self.number);
    }
}

impl embedded_hal::digital::ErrorType for GpioOutput<'_> {
    type Error = Error;
}

impl embedded_hal::digital::OutputPin for GpioOutput<'_> {
    fn set_low(&mut self) -> Result<()> {
        self.pin.set_low();
        Ok(())
    }

    fn set_high(&mut self) -> Result<()> {
        self.pin.set_high();
        Ok(())
    }
}

impl embedded_hal::digital::StatefulOutputPin for GpioOutput<'_> {
    fn is_set_high(&mut self) -> Result<bool> {
        Ok(self.pin.is_set_high())
    }

    fn is_set_low(&mut self) -> Result<bool> {
        Ok(!self.pin.is_set_high())
    }
}

/// Mounts `/devices/gpio/pin<number>` for every line `gpio` exposes.
pub async fn mount_pin_devices(
    virtual_file_system: &VirtualFileSystem,
    task: TaskIdentifier,
    gpio: &'static Gpio,
) -> Result<()> {
    for line in gpio.lines() {
        mount_gpio(
            virtual_file_system,
            task,
            line.name,
            gpio,
            LineId::new(u16::from(line.number)),
        )
        .await
        .map_err(|_| Error::FailedToMountPinDevice)?;
    }

    Ok(())
}
