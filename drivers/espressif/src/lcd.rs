//! 8080 parallel display bus over esp-hal's LCD_CAM peripheral, with DMA
//! (feature `lcd-i8080`, ESP32-S3).
//!
//! [`I8080Bus`] implements [`ParallelBus`], so `mipidsi` drives the panel
//! through `drivers_shared::devices::display::ParallelInterface`. Each write
//! is one LCD_CAM transaction: the optional command byte, then the data,
//! straight from the caller's slice through DMA descriptors. A slice in
//! internal SRAM or PSRAM is not copied; data elsewhere (constants in flash)
//! goes through a small staging buffer in internal SRAM.

use drivers_shared::devices::display::ParallelBus;
use esp_hal::{
    Blocking,
    dma::{BurstConfig, DmaDescriptor, DmaError, DmaTxBuffer, Preparation, aligned::DmaAlignedRef},
    lcd_cam::lcd::i8080::{Command, I8080},
};

/// Bytes per DMA descriptor: at most 4095, kept a multiple of 4.
pub const DESCRIPTOR_CHUNK_SIZE: usize = 4092;

/// Bytes of short writes, such as command parameters, copied to internal SRAM
/// when they are not already there.
const STAGING_SIZE: usize = 64;

/// Internal data RAM of the ESP32-S3, which DMA reads without cache
/// maintenance.
const INTERNAL_DATA_RAM: core::ops::Range<usize> = 0x3FC8_8000..0x3FD0_0000;

fn is_internal(data: &[u8]) -> bool {
    let start = data.as_ptr() as usize;
    INTERNAL_DATA_RAM.contains(&start) && start + data.len() <= INTERNAL_DATA_RAM.end
}

/// A DMA transmit buffer over borrowed data: the caller waits for the
/// transfer before the borrow ends.
struct BorrowedTransmitBuffer<'a> {
    descriptors: &'a mut [DmaDescriptor],
    data: &'a [u8],
}

impl<'a> BorrowedTransmitBuffer<'a> {
    /// Links enough of `descriptors` to cover `data`; `None` if there are too
    /// few.
    fn new(descriptors: &'a mut [DmaDescriptor], data: &'a [u8]) -> Option<Self> {
        let count = data.len().div_ceil(DESCRIPTOR_CHUNK_SIZE).max(1);
        let descriptors = descriptors.get_mut(..count)?;
        let mut next = core::ptr::null_mut();
        for (index, descriptor) in descriptors.iter_mut().enumerate().rev() {
            let start = (index * DESCRIPTOR_CHUNK_SIZE).min(data.len());
            let end = (start + DESCRIPTOR_CHUNK_SIZE).min(data.len());
            // The DMA only reads through this pointer.
            descriptor.buffer = data[start..].as_ptr().cast_mut();
            descriptor.set_size(end - start);
            descriptor.set_length(end - start);
            descriptor.next = next;
            next = descriptor;
        }
        Some(Self { descriptors, data })
    }
}

// SAFETY: the descriptors cover exactly `data`, which outlives the transfer:
// `I8080Bus::transfer` waits for the transfer to end before it returns.
unsafe impl DmaTxBuffer for BorrowedTransmitBuffer<'_> {
    type View = Self;
    type Final = ();

    fn prepare(&mut self) -> Preparation {
        let last = self.descriptors.len() - 1;
        for (index, descriptor) in self.descriptors.iter_mut().enumerate() {
            descriptor.reset_for_tx(index == last);
        }
        // Push the CPU's writes out of the cache before the DMA reads PSRAM.
        // SAFETY: writing back cache lines does not change any data.
        unsafe { DmaAlignedRef::new_unchecked(self.data) }.writeback();
        Preparation {
            start: self.descriptors.as_mut_ptr(),
            accesses_psram: !is_internal(self.data),
            burst_transfer: BurstConfig::default(),
            check_owner: None,
            auto_write_back: false,
        }
    }

    fn into_view(self) -> Self {
        self
    }

    fn from_view(_: Self) {}
}

/// An LCD_CAM bus error.
#[derive(Debug)]
pub enum I8080Error {
    /// The DMA transfer failed.
    Dma(DmaError),
    /// A previous transfer failed in a way that lost the peripheral.
    Unavailable,
}

/// The 8080 bus: esp-hal's [`I8080`] driver and the DMA descriptors that its
/// transfers use.
pub struct I8080Bus {
    i8080: Option<I8080<'static, Blocking>>,
    descriptors: &'static mut [DmaDescriptor],
}

impl I8080Bus {
    /// Takes a configured [`I8080`] and descriptors in internal SRAM, which
    /// bound one transfer to `descriptors.len() * DESCRIPTOR_CHUNK_SIZE`
    /// bytes; longer writes are split into several transfers.
    pub fn new(i8080: I8080<'static, Blocking>, descriptors: &'static mut [DmaDescriptor]) -> Self {
        assert!(!descriptors.is_empty(), "the bus needs DMA descriptors");
        Self {
            i8080: Some(i8080),
            descriptors,
        }
    }

    fn capacity(&self) -> usize {
        self.descriptors.len() * DESCRIPTOR_CHUNK_SIZE
    }

    fn transfer(&mut self, command: Command<u8>, data: &[u8]) -> Result<(), I8080Error> {
        let buffer =
            BorrowedTransmitBuffer::new(self.descriptors, data).ok_or(I8080Error::Unavailable)?;
        let i8080 = self.i8080.take().ok_or(I8080Error::Unavailable)?;
        let (result, i8080) = match i8080.send(command, 0, buffer) {
            Ok(transfer) => {
                let (result, i8080, ()) = transfer.wait();
                (result, i8080)
            }
            Err((error, i8080, _)) => (Err(error), i8080),
        };
        self.i8080 = Some(i8080);
        result.map_err(I8080Error::Dma)
    }
}

impl ParallelBus for I8080Bus {
    type Error = I8080Error;

    fn write(&mut self, command: Option<u8>, data: &[u8]) -> Result<(), Self::Error> {
        let mut command = command.map_or(Command::None, Command::One);
        if data.is_empty() {
            return match command {
                Command::None => Ok(()),
                _ => self.transfer(command, &[]),
            };
        }
        // Short data that may live in flash, where DMA cannot read: copy it to
        // the stack, in internal SRAM.
        if data.len() <= STAGING_SIZE && !is_internal(data) {
            let mut staging = [0; STAGING_SIZE];
            staging[..data.len()].copy_from_slice(data);
            return self.transfer(command, &staging[..data.len()]);
        }
        for chunk in data.chunks(self.capacity()) {
            self.transfer(command, chunk)?;
            command = Command::None;
        }
        Ok(())
    }
}
