//! I2C bus controller over esp-hal's master driver.
//!
//! The bus is owned by [`EspI2c`]. Opening it takes the esp-hal driver out of a
//! critical-section mutex and returns it when the [`EspI2cBus`] is dropped, so
//! at most one transaction can run at a time without any unsafe code. 10-bit
//! addressing is not supported by the esp-hal master driver and is reported as
//! unsupported.

use core::cell::RefCell;

use critical_section::Mutex;
use embedded_hal::i2c::{ErrorKind, ErrorType, Operation, SevenBitAddress, TenBitAddress};
use esp_hal::{
    Blocking,
    i2c::master::{Config, Error as HalError, I2c as HalI2c},
    time::Rate,
};
use peripherals::i2c::{BusConfig, Capabilities, Error, I2cController, NoAcknowledge};

/// Highest bus frequency the platform allows. Standard-mode Fast-mode Plus is
/// not validated on the boards yet, so 400 kHz is the limit.
pub const MAX_FREQUENCY_HZ: u32 = 400_000;

/// Owned I2C master. Created once the pins are assigned in chip initialisation.
pub struct EspI2c {
    bus: Mutex<RefCell<Option<HalI2c<'static, Blocking>>>>,
}

impl EspI2c {
    pub fn new(bus: HalI2c<'static, Blocking>) -> Self {
        Self {
            bus: Mutex::new(RefCell::new(Some(bus))),
        }
    }
}

impl I2cController for EspI2c {
    type Bus<'a>
        = EspI2cBus<'a>
    where
        Self: 'a;

    fn capabilities(&self) -> Capabilities {
        Capabilities {
            ten_bit: false,
            max_frequency_hz: MAX_FREQUENCY_HZ,
        }
    }

    fn open(&self, config: BusConfig) -> Result<EspI2cBus<'_>, Error> {
        if config.frequency_hz == 0 || config.frequency_hz > MAX_FREQUENCY_HZ {
            return Err(Error::Unsupported);
        }

        let taken = critical_section::with(|cs| self.bus.borrow(cs).borrow_mut().take())
            .ok_or(Error::Busy)?;

        // The bus returns to the controller when `bus` is dropped, including
        // when the frequency cannot be applied below.
        let mut bus = EspI2cBus {
            controller: self,
            bus: Some(taken),
        };
        let hal_config = Config::default().with_frequency(Rate::from_hz(config.frequency_hz));
        bus.bus
            .as_mut()
            .ok_or(Error::Busy)?
            .apply_config(&hal_config)
            .map_err(|_| Error::Unsupported)?;

        Ok(bus)
    }
}

/// Reserved bus. Dropping it returns the esp-hal driver to its controller.
pub struct EspI2cBus<'a> {
    controller: &'a EspI2c,
    bus: Option<HalI2c<'static, Blocking>>,
}

impl Drop for EspI2cBus<'_> {
    fn drop(&mut self) {
        if let Some(bus) = self.bus.take() {
            critical_section::with(|cs| {
                *self.controller.bus.borrow(cs).borrow_mut() = Some(bus);
            });
        }
    }
}

impl ErrorType for EspI2cBus<'_> {
    type Error = Error;
}

impl embedded_hal::i2c::I2c<SevenBitAddress> for EspI2cBus<'_> {
    fn transaction(
        &mut self,
        address: SevenBitAddress,
        operations: &mut [Operation<'_>],
    ) -> Result<(), Error> {
        let bus = self.bus.as_mut().ok_or(Error::Busy)?;
        embedded_hal::i2c::I2c::<SevenBitAddress>::transaction(bus, address, operations)
            .map_err(map_error)
    }
}

impl embedded_hal::i2c::I2c<TenBitAddress> for EspI2cBus<'_> {
    fn transaction(
        &mut self,
        _address: TenBitAddress,
        _operations: &mut [Operation<'_>],
    ) -> Result<(), Error> {
        Err(Error::Unsupported)
    }
}

/// Maps an esp-hal master error to the peripheral contract error.
fn map_error(error: HalError) -> Error {
    if let HalError::Timeout = error {
        return Error::Timeout;
    }

    match embedded_hal::i2c::Error::kind(&error) {
        ErrorKind::NoAcknowledge(source) => Error::NoAcknowledge(match source {
            embedded_hal::i2c::NoAcknowledgeSource::Address => NoAcknowledge::Address,
            embedded_hal::i2c::NoAcknowledgeSource::Data => NoAcknowledge::Data,
            _ => NoAcknowledge::Unknown,
        }),
        ErrorKind::ArbitrationLoss => Error::ArbitrationLoss,
        ErrorKind::Overrun | ErrorKind::Bus => Error::Bus,
        _ => Error::Other,
    }
}
