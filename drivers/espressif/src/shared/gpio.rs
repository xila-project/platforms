use core::cell::Cell;

use critical_section::Mutex;
use drivers_core::GpioDevice;
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

/// One physical GPIO line of a chip, its VFS path and the features it supports.
#[derive(Clone, Copy, Debug)]
pub struct Line {
    pub number: u8,
    pub path: &'static str,
    pub capabilities: LineCapabilities,
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

/// Declares a line of `FULL_CAPABILITIES`, mounted at `/devices/Pin<number>`.
#[macro_export]
macro_rules! pin_line {
    ($number:literal) => {
        $crate::shared::Line {
            number: $number,
            path: concat!("/devices/Pin", $number),
            capabilities: $crate::shared::FULL_CAPABILITIES,
        }
    };
}

/// Declares a line of `INPUT_ONLY_CAPABILITIES`, mounted at `/devices/Pin<number>`.
#[macro_export]
macro_rules! pin_input_only {
    ($number:literal) => {
        $crate::shared::Line {
            number: $number,
            path: concat!("/devices/Pin", $number),
            capabilities: $crate::shared::INPUT_ONLY_CAPABILITIES,
        }
    };
}

/// ESP GPIO controller implementing the portable `GpioController` contract.
///
/// `LineId` is the physical GPIO number. A bit per GPIO number records which
/// lines have a live handle, so one pin cannot be requested twice.
pub struct Gpio {
    lines: &'static [Line],
    reserved: Mutex<Cell<u64>>,
}

impl Gpio {
    pub const fn new(lines: &'static [Line]) -> Self {
        Self {
            lines,
            reserved: Mutex::new(Cell::new(0)),
        }
    }

    /// Every line of the chip, in the order they are mounted.
    pub const fn lines(&self) -> &'static [Line] {
        self.lines
    }

    fn line(&self, id: LineId) -> Result<Line> {
        self.lines
            .iter()
            .copied()
            .find(|line| u16::from(line.number) == id.offset())
            .ok_or(Error::InvalidLine)
    }

    fn reserve(&self, number: u8) -> Result<()> {
        let bit = reservation_bit(number)?;

        critical_section::with(|cs| {
            let reserved = self.reserved.borrow(cs);
            let bits = reserved.get();

            if bits & bit != 0 {
                return Err(Error::AlreadyReserved);
            }

            reserved.set(bits | bit);
            Ok(())
        })
    }

    fn release(&self, number: u8) {
        if let Ok(bit) = reservation_bit(number) {
            critical_section::with(|cs| {
                let reserved = self.reserved.borrow(cs);
                reserved.set(reserved.get() & !bit);
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

        // SAFETY: the reservation above is the only live handle to this GPIO
        // number, and it is released when the returned `GpioInput` is dropped.
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

/// Mounts one character device per GPIO line of `controller` in the VFS.
pub async fn mount_pin_devices(
    virtual_file_system: &VirtualFileSystem,
    task: TaskIdentifier,
    controller: &'static Gpio,
) -> Result<()> {
    for line in controller.lines() {
        let device = GpioDevice::new(controller, LineId::new(u16::from(line.number)));

        virtual_file_system
            .mount_character_device(task, line.path, device)
            .await
            .map_err(|_| Error::FailedToMountPinDevice)?;
    }

    Ok(())
}
