//! GPIO line character device for Nordic nRF52840.
//!
//! Exposes an individual pin as a VFS character device under `/devices/gpio/<name>`.

use core::task::Poll;

use embassy_nrf::gpio::{AnyPin, Flex, OutputDrive, Pull as NrfPull};
use file_system::{
    ControlCommand, ControlCommandIdentifier, DirectBaseOperations, DirectCharacterDevice, Error,
    MountOperations, Result, Size, mount::MutexMountWrapper,
};
use shared::AnyByLayout;
use synchronization::blocking_mutex::raw::CriticalSectionRawMutex;

use super::logic::{
    Direction, DriveMode, Level, Pull, decode_write_buffer, encode_read_level, get_capabilities,
    is_pin_reserved, validate_configure, validate_pin, validate_version, version_abi,
};
use device::gpio::{CONFIGURE, GET_CAPABILITIES, RELEASE};

struct PinStateInner {
    flex: Option<Flex<'static>>,
    direction: Direction,
}

/// A character device exposing a single GPIO pin of the nRF52840.
pub struct GpioPinDevice {
    pin_index: u8,
    reserved_mask: u64,
    inner: MutexMountWrapper<CriticalSectionRawMutex, PinStateInner>,
}

impl GpioPinDevice {
    /// Create a new `GpioPinDevice` for the given pin index (0..47).
    ///
    /// The pin starts unconfigured (high impedance).
    /// Returns `Error::AccessDenied` if the pin is marked reserved, or `Error::InvalidParameter`
    /// if the pin index is >= 48.
    pub fn new(pin_index: u8, reserved_mask: u64) -> Result<Self> {
        validate_pin(pin_index, reserved_mask)?;
        Ok(Self {
            pin_index,
            reserved_mask,
            inner: MutexMountWrapper::new_mounted(PinStateInner {
                flex: None,
                direction: Direction::Input,
            }),
        })
    }

    /// Return the flat pin index (0..47).
    pub const fn pin_index(&self) -> u8 {
        self.pin_index
    }

    /// Configure the pin mode, pull, and initial output state.
    pub fn configure_pin(
        &self,
        direction: Direction,
        pull: Pull,
        output: Option<(DriveMode, Level)>,
    ) -> Result<()> {
        if is_pin_reserved(self.pin_index, self.reserved_mask) {
            return Err(Error::PermissionDenied);
        }

        let mut inner = self.inner.try_get()?;
        // Dropping any existing Flex returns the pin to disconnected.
        inner.flex = None;

        let any_pin = unsafe { AnyPin::steal(self.pin_index) };
        let mut flex = Flex::new(any_pin);

        match direction {
            Direction::Input => {
                let nrf_pull = match pull {
                    Pull::Disabled => NrfPull::None,
                    Pull::Up => NrfPull::Up,
                    Pull::Down => NrfPull::Down,
                };
                flex.set_as_input(nrf_pull);
            }
            Direction::Output => {
                let (drive_mode, initial_level) = output.ok_or(Error::InvalidParameter)?;
                match initial_level {
                    Level::Low => flex.set_low(),
                    Level::High => flex.set_high(),
                }
                match drive_mode {
                    DriveMode::PushPull => {
                        flex.set_as_output(OutputDrive::Standard);
                    }
                    DriveMode::OpenDrain => {
                        let nrf_pull = match pull {
                            Pull::Disabled => NrfPull::None,
                            Pull::Up => NrfPull::Up,
                            Pull::Down => NrfPull::Down,
                        };
                        flex.set_as_input_output(nrf_pull, OutputDrive::Standard0Disconnect1);
                    }
                }
            }
        }

        inner.direction = direction;
        inner.flex = Some(flex);
        Ok(())
    }

    /// Release the pin, returning it to disconnected (high impedance).
    pub fn release_pin(&self) -> Result<()> {
        let mut inner = self.inner.try_get()?;
        inner.flex = None;
        Ok(())
    }
}

impl DirectBaseOperations for GpioPinDevice {
    fn read(
        &self,
        _cx: &mut core::task::Context<'_>,
        buffer: &mut [u8],
        absolute_position: Size,
    ) -> Poll<Result<usize>> {
        // A pin reads as the text `0\n` or `1\n`, then end of file so `cat` terminates.
        let Ok(offset) = usize::try_from(absolute_position) else {
            return Poll::Ready(Ok(0));
        };
        if offset >= 2 {
            return Poll::Ready(Ok(0));
        }
        file_system::ready(|| {
            let mut inner = self.inner.try_get()?;

            // If unconfigured, auto-configure as input so reading works directly from the shell.
            if inner.flex.is_none() {
                drop(inner);
                self.configure_pin(Direction::Input, Pull::Disabled, None)?;
                inner = self.inner.try_get()?;
            }

            let direction = inner.direction;
            let flex = inner.flex.as_mut().ok_or(Error::NotInitialized)?;
            let level = match direction {
                Direction::Input => flex.is_high(),
                Direction::Output => flex.is_set_high(),
            };
            let text = &encode_read_level(level)[offset..];
            let count = text.len().min(buffer.len());
            buffer[..count].copy_from_slice(&text[..count]);
            Ok(count)
        })
    }

    fn write(
        &self,
        _cx: &mut core::task::Context<'_>,
        buffer: &[u8],
        _absolute_position: Size,
    ) -> Poll<Result<usize>> {
        file_system::ready(|| {
            let Some(level) = decode_write_buffer(buffer)? else {
                return Ok(buffer.len());
            };
            let mut inner = self.inner.try_get()?;

            // If explicitly configured as input via control, reject writes.
            if inner.flex.is_some() && inner.direction != Direction::Output {
                return Err(Error::UnsupportedOperation);
            }

            // If unconfigured, auto-configure as push-pull output with the requested level.
            if inner.flex.is_none() {
                drop(inner);
                self.configure_pin(
                    Direction::Output,
                    Pull::Disabled,
                    Some((DriveMode::PushPull, level)),
                )?;
                return Ok(buffer.len());
            }

            let flex = inner.flex.as_mut().ok_or(Error::NotInitialized)?;
            match level {
                Level::Low => flex.set_low(),
                Level::High => flex.set_high(),
            }
            Ok(buffer.len())
        })
    }

    fn control(
        &self,
        _cx: &mut core::task::Context<'_>,
        command: ControlCommandIdentifier,
        input: &AnyByLayout,
        output: &mut AnyByLayout,
    ) -> Poll<Result<()>> {
        file_system::ready(|| {
            if command == CONFIGURE::IDENTIFIER {
                let request = CONFIGURE::cast_input(input)?;
                let config = validate_configure(request)?;
                self.configure_pin(config.direction, config.pull, config.output)?;
                *CONFIGURE::cast_output(output)? = version_abi();
            } else if command == RELEASE::IDENTIFIER {
                let request = RELEASE::cast_input(input)?;
                validate_version(request)?;
                self.release_pin()?;
                *RELEASE::cast_output(output)? = version_abi();
            } else if command == GET_CAPABILITIES::IDENTIFIER {
                let request = GET_CAPABILITIES::cast_input(input)?;
                validate_version(request)?;
                *GET_CAPABILITIES::cast_output(output)? = get_capabilities();
            } else {
                return Err(Error::UnsupportedOperation);
            }
            Ok(())
        })
    }
}

impl MountOperations for GpioPinDevice {
    fn unmount(&self) -> Result<()> {
        self.inner.unmount()
    }
}

impl DirectCharacterDevice for GpioPinDevice {}
