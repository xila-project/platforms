pub type Result<T> = core::result::Result<T, Error>;

/// Errors reported by the ESP GPIO backend.
///
/// Variant names match `peripherals::gpio::MockError` so that tests and
/// hardware backends report the same conditions.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    /// The requested line does not exist on this chip.
    InvalidLine,
    /// The line is already requested by another live handle.
    AlreadyReserved,
    /// The chip does not support the requested direction, pull or drive mode.
    Unsupported,
    /// A line's device could not be mounted in the virtual file system.
    FailedToMountPinDevice,
}

impl embedded_hal::digital::Error for Error {
    fn kind(&self) -> embedded_hal::digital::ErrorKind {
        embedded_hal::digital::ErrorKind::Other
    }
}
