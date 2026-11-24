use mipidsi::interface::{Interface, InterfaceKind};

/// An 8-bit parallel (8080) display bus.
pub trait ParallelBus {
    type Error: core::fmt::Debug;

    /// Writes `command`, when present, with the data/command line low, then
    /// `data` with it high, as one transaction.
    fn write(&mut self, command: Option<u8>, data: &[u8]) -> Result<(), Self::Error>;
}

impl<Bus: ParallelBus> ParallelBus for &mut Bus {
    type Error = Bus::Error;

    fn write(&mut self, command: Option<u8>, data: &[u8]) -> Result<(), Self::Error> {
        Bus::write(self, command, data)
    }
}

/// Bytes of pixels that `mipidsi` converts itself (for example a fill) staged
/// per bus write.
pub const STAGING_SIZE: usize = 512;

/// A `mipidsi` interface over a [`ParallelBus`].
///
/// - Commands and their parameters go to the bus in one write.
/// - Raw pixels, already in the panel's wire format, go to the bus in one
///   write of the caller's slice, without copying: with a DMA bus, that is one
///   DMA transfer.
/// - Pixels that `mipidsi` converts itself are staged in a small buffer.
pub struct ParallelInterface<Bus> {
    bus: Bus,
    staging: [u8; STAGING_SIZE],
}

impl<Bus: ParallelBus> ParallelInterface<Bus> {
    pub const fn new(bus: Bus) -> Self {
        Self {
            bus,
            staging: [0; STAGING_SIZE],
        }
    }

    /// Gives the bus back.
    pub fn release(self) -> Bus {
        self.bus
    }
}

impl<Bus: ParallelBus> Interface for ParallelInterface<Bus> {
    type Word = u8;
    type Error = Bus::Error;

    const KIND: InterfaceKind = InterfaceKind::Parallel8Bit;

    fn send_command(&mut self, command: u8, arguments: &[u8]) -> Result<(), Self::Error> {
        self.bus.write(Some(command), arguments)
    }

    fn send_pixels<const N: usize>(
        &mut self,
        pixels: impl IntoIterator<Item = [Self::Word; N]>,
    ) -> Result<(), Self::Error> {
        let mut length = 0;
        for pixel in pixels {
            if length + N > STAGING_SIZE {
                self.bus.write(None, &self.staging[..length])?;
                length = 0;
            }
            self.staging[length..length + N].copy_from_slice(&pixel);
            length += N;
        }
        if length != 0 {
            self.bus.write(None, &self.staging[..length])?;
        }
        Ok(())
    }

    fn send_repeated_pixel<const N: usize>(
        &mut self,
        pixel: [Self::Word; N],
        count: u32,
    ) -> Result<(), Self::Error> {
        self.send_pixels(core::iter::repeat_n(pixel, count as usize))
    }

    fn send_raw_pixels(&mut self, data: &[Self::Word]) -> Result<(), Self::Error> {
        if data.is_empty() {
            return Ok(());
        }
        self.bus.write(None, data)
    }
}

/// A 4-wire serial display bus (such as SPI with DMA) that can overlap an
/// asynchronous pixel transfer with CPU rendering into a second draw buffer.
pub trait AsynchronousDisplayBus {
    type Error: core::fmt::Debug;

    /// Sends a command byte and optional parameters synchronously, waiting for
    /// any earlier asynchronous transfer to finish first.
    fn send_command(&mut self, command: u8, arguments: &[u8]) -> Result<(), Self::Error>;

    /// Sends raw pixel bytes synchronously, safe for caller-owned stack or
    /// temporary buffers.
    fn send_pixels(&mut self, data: &[u8]) -> Result<(), Self::Error>;

    /// Sets the panel address window (`0x2A`, `0x2B`, `0x2C`) and starts
    /// sending `data`.
    ///
    /// Returns `Ok(true)` when an asynchronous transfer is now in flight and
    /// must be finished by [`wait_for_transfer`](Self::wait_for_transfer)
    /// before `data` is reused, or `Ok(false)` when `data` has already been
    /// sent synchronously.
    ///
    /// # Safety
    ///
    /// When this returns `Ok(true)`, the memory behind `data` must stay valid
    /// and unmodified until [`wait_for_transfer`](Self::wait_for_transfer)
    /// returns (or `self` is dropped).
    unsafe fn start_window_pixels(
        &mut self,
        x_start: u16,
        y_start: u16,
        x_end: u16,
        y_end: u16,
        data: &[u8],
    ) -> Result<bool, Self::Error>;

    /// Waits for any in-flight asynchronous pixel transfer to finish.
    fn wait_for_transfer(&mut self) -> Result<(), Self::Error>;
}

/// A `mipidsi` 4-wire serial interface over an [`AsynchronousDisplayBus`], used for
/// synchronous panel initialisation and clearing before handing the bus to
/// [`super::AsynchronousScreenDevice`].
pub struct SerialInterface<Bus> {
    bus: Bus,
}

impl<Bus: AsynchronousDisplayBus> SerialInterface<Bus> {
    pub const fn new(bus: Bus) -> Self {
        Self { bus }
    }

    /// Gives the underlying bus back once `mipidsi` initialisation is done.
    pub fn release(self) -> Bus {
        self.bus
    }
}

impl<Bus: AsynchronousDisplayBus> Interface for SerialInterface<Bus> {
    type Word = u8;
    type Error = Bus::Error;

    const KIND: InterfaceKind = InterfaceKind::Serial4Line;

    fn send_command(&mut self, command: u8, arguments: &[u8]) -> Result<(), Self::Error> {
        self.bus.send_command(command, arguments)
    }

    fn send_pixels<const N: usize>(
        &mut self,
        pixels: impl IntoIterator<Item = [Self::Word; N]>,
    ) -> Result<(), Self::Error> {
        let mut staging = [0u8; 64];
        let mut length = 0;
        for pixel in pixels {
            if length + N > staging.len() {
                self.bus.send_pixels(&staging[..length])?;
                length = 0;
            }
            staging[length..length + N].copy_from_slice(&pixel);
            length += N;
        }
        if length != 0 {
            self.bus.send_pixels(&staging[..length])?;
        }
        Ok(())
    }

    fn send_repeated_pixel<const N: usize>(
        &mut self,
        pixel: [Self::Word; N],
        count: u32,
    ) -> Result<(), Self::Error> {
        self.send_pixels(core::iter::repeat_n(pixel, count as usize))
    }

    fn send_raw_pixels(&mut self, data: &[Self::Word]) -> Result<(), Self::Error> {
        if data.is_empty() {
            return Ok(());
        }
        self.bus.send_pixels(data)
    }
}
