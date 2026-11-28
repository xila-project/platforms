use core::task::{Context, Poll};

use file_system::{DirectBaseOperations, DirectCharacterDevice, MountOperations, Size};
use io_error::{Kind, Result};

/// Time device for boards without a battery-backed or configured wall clock.
///
/// It does not invent an arbitrary date: reads report that the device is
/// unavailable and the real-time source reports `Unsynchronized`, so the
/// runtime can start with `time::Manager::unsynchronized()`.
#[derive(Clone, Copy, Debug, Default)]
pub struct UnsynchronizedTimeDevice;

impl UnsynchronizedTimeDevice {
    pub const fn new() -> Self {
        Self
    }
}

impl DirectBaseOperations for UnsynchronizedTimeDevice {
    fn read(&self, _: &mut Context<'_>, _: &mut [u8], _: Size) -> Poll<Result<usize>> {
        Poll::Ready(Err(Kind::DeviceUnavailable))
    }

    fn write(&self, _: &mut Context<'_>, _: &[u8], _: Size) -> Poll<Result<usize>> {
        Poll::Ready(Err(Kind::Unsupported))
    }
}

impl MountOperations for UnsynchronizedTimeDevice {}

impl DirectCharacterDevice for UnsynchronizedTimeDevice {}

impl time::RealtimeSource for UnsynchronizedTimeDevice {
    fn read(&self) -> time::Result<time::RealtimeSample> {
        Err(time::Error::Unsynchronized)
    }
}
