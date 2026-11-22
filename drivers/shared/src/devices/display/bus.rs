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
