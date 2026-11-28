use core::task::{Context, Poll};

use file_system::{DirectBaseOperations, DirectCharacterDevice, MountOperations, Position, Size};
use io_error::{Kind, Result};

#[cfg(any(
    feature = "esp32",
    feature = "esp32c2",
    feature = "esp32c3",
    feature = "esp32c6",
    feature = "esp32h2",
    feature = "esp32s2",
    feature = "esp32s3",
))]
mod hardware;

/// Character device backed by a cryptographically secure source of random bytes.
pub struct RandomDevice {
    fill: fn(&mut [u8]) -> Result<()>,
}

impl RandomDevice {
    /// Wraps a board-provided entropy source.
    ///
    /// The callback must fill the entire buffer with cryptographically secure
    /// random bytes or return an error. It must be safe to call concurrently.
    /// Boards using the ESP32-C5, ESP32-C61, ESP32-P4 or ESP32-S31 need this
    /// constructor because esp-hal 1.2.2 cannot enable hardware entropy there.
    pub const fn from_entropy_source(fill: fn(&mut [u8]) -> Result<()>) -> Self {
        Self { fill }
    }
}

impl DirectBaseOperations for RandomDevice {
    fn read(&self, _: &mut Context<'_>, buffer: &mut [u8], _: Size) -> Poll<Result<usize>> {
        // Bound the sampling time of a single poll, as in the Nordic driver.
        let count = buffer.len().min(32);
        file_system::ready(|| {
            if count != 0 {
                (self.fill)(&mut buffer[..count])?;
            }
            Ok(count)
        })
    }

    fn write(&self, _: &mut Context<'_>, _: &[u8], _: Size) -> Poll<Result<usize>> {
        Poll::Ready(Err(Kind::Unsupported))
    }

    fn set_position(&self, _: Size, _: &Position) -> Result<Size> {
        Err(Kind::Unsupported)
    }
}

impl MountOperations for RandomDevice {}
impl DirectCharacterDevice for RandomDevice {}
