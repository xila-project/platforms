use file_system::{
    DirectBaseOperations, DirectCharacterDevice, Error, MountOperations, Result, Size,
};

/// Hardware time character device for nRF52840.
///
/// Because the nRF52840 has no battery-backed wall clock / epoch calendar,
/// this device does not invent an arbitrary date and reports `Unsynchronized`.
#[derive(Clone, Copy, Debug, Default)]
pub struct TimeDevice;

impl TimeDevice {
    pub const fn new() -> Self {
        Self
    }
}

impl DirectBaseOperations for TimeDevice {
    fn read(
        &self,
        _cx: &mut core::task::Context<'_>,
        _buffer: &mut [u8],
        _: Size,
    ) -> core::task::Poll<Result<usize>> {
        // No hardware real-time epoch is present without external synchronization.
        file_system::ready(|| Err(Error::TimeError))
    }

    fn write(
        &self,
        _cx: &mut core::task::Context<'_>,
        _: &[u8],
        _: Size,
    ) -> core::task::Poll<Result<usize>> {
        file_system::ready(|| Err(Error::UnsupportedOperation))
    }
}

impl MountOperations for TimeDevice {}

impl DirectCharacterDevice for TimeDevice {}

impl time::RealtimeSource for TimeDevice {
    fn read(&self) -> time::Result<time::RealtimeSample> {
        // RealtimeSource contract: return an error rather than fabricate an epoch.
        // The runtime can start with `time::Manager::unsynchronized()`.
        Err(time::Error::Unsynchronized)
    }
}
