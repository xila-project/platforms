use core::cmp::min;
use core::task::Poll;
use embassy_nrf::Peri;
use embassy_nrf::mode::Blocking;
use embassy_nrf::peripherals::RNG;
use embassy_nrf::rng::Rng;
use file_system::{
    DirectBaseOperations, DirectCharacterDevice, Error, MountOperations, Result, Size,
    mount::MutexMountWrapper,
};
use synchronization::blocking_mutex::raw::CriticalSectionRawMutex;

struct RandomInner {
    rng: Rng<'static, Blocking>,
}

/// Hardware random number generator VFS character device for Nordic nRF52840.
pub struct RandomDevice {
    inner: MutexMountWrapper<CriticalSectionRawMutex, RandomInner>,
}

impl RandomDevice {
    /// Maximum bytes generated in a single polling read (~3.84 ms worst-case latency).
    pub const MAX_BATCH_SIZE: usize = 32;

    /// Create a new `RandomDevice` from the RNG peripheral with hardware bias correction enabled.
    pub fn new(rng: Peri<'static, RNG>) -> Self {
        let rng = Rng::new_blocking(rng);
        rng.set_bias_correction(true);
        Self {
            inner: MutexMountWrapper::new_mounted(RandomInner { rng }),
        }
    }
}

impl DirectBaseOperations for RandomDevice {
    fn read(
        &self,
        _cx: &mut core::task::Context<'_>,
        buffer: &mut [u8],
        _position: Size,
    ) -> Poll<Result<usize>> {
        file_system::ready(|| {
            if buffer.is_empty() {
                return Ok(0);
            }
            let mut inner = self.inner.try_get()?;
            let to_read = min(buffer.len(), Self::MAX_BATCH_SIZE);
            inner.rng.blocking_fill_bytes(&mut buffer[..to_read]);
            Ok(to_read)
        })
    }

    fn write(
        &self,
        _cx: &mut core::task::Context<'_>,
        _buffer: &[u8],
        _position: Size,
    ) -> Poll<Result<usize>> {
        file_system::ready(|| Err(Error::UnsupportedOperation))
    }

    fn set_position(&self, _position: Size, _new_position: &file_system::Position) -> Result<Size> {
        Err(Error::UnsupportedOperation)
    }
}

impl MountOperations for RandomDevice {
    fn unmount(&self) -> Result<()> {
        self.inner.unmount()
    }
}

impl DirectCharacterDevice for RandomDevice {}
