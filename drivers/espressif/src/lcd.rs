//! 8080 parallel display bus over esp-hal's LCD_CAM peripheral, with DMA
//! (feature `lcd-i8080`, ESP32-S3).
//!
//! [`I8080Bus`] implements [`ParallelBus`], so `mipidsi` drives the panel
//! through `drivers_shared::devices::display::ParallelInterface`. Each write
//! is one LCD_CAM transaction: the optional command byte, then the data,
//! straight from the caller's slice through DMA descriptors. Each slice is
//! classified first:
//!
//! - in internal SRAM: sent as it is, with no cache maintenance;
//! - in PSRAM, with the address and length aligned to the data cache line, as
//!   esp-hal's checked [`DmaAlignedRef::new`] requires: its cache lines are
//!   written back once, then it is sent as it is;
//! - anywhere else, such as constants in flash, which the DMA cannot read, or
//!   unaligned PSRAM: copied through a staging buffer on the stack, in internal
//!   SRAM, one chunk per transfer.
//!
//! # Setup cycles
//!
//! ESP32-S3 erratum LCD-239: in the 8080 format, the first data value and the
//! number of data values that the peripheral sends can be wrong if the data
//! phase starts two LCD_CLK cycles or less after the transaction starts. The
//! count is `(setup + command + dummy cycles) * (LCD_CLK cycles per LCD_PCLK
//! cycle)`. esp-hal divides LCD_CLK by two, and the bus sends no dummy cycle.
//! A write without a command, such as the pixels that follow `RAMWR`, thus
//! has only its setup cycles before its data. With esp-hal's default of one
//! setup cycle, the count is two: the panel can receive a wrong first pixel,
//! or one pixel more or less than the address window holds. The image of a
//! partial update then moves by one pixel in its window, which shows as a
//! line on the left or right edge of the updated area.
//!
//! [`configuration`] sets [`SETUP_CYCLES`] setup cycles, so every transaction
//! satisfies the erratum. Build the [`I8080`] driver with it.

use core::marker::PhantomData;

use drivers_shared::devices::display::ParallelBus;
use esp_hal::{
    Blocking,
    dma::{BurstConfig, DmaDescriptor, DmaError, DmaTxBuffer, Preparation, aligned::DmaAlignedRef},
    lcd_cam::lcd::i8080::{Command, Config, I8080},
    time::Rate,
};

/// Bytes per DMA descriptor: at most 4095, kept a multiple of 4.
pub const DESCRIPTOR_CHUNK_SIZE: usize = 4092;

/// LCD_CLK cycles per LCD_PCLK cycle: esp-hal always divides LCD_CLK by two.
const CLOCK_CYCLES_PER_BUS_CYCLE: usize = 2;

/// Setup cycles before each transaction, in LCD_PCLK cycles. The write clock
/// does not pulse during them. See the module documentation.
pub const SETUP_CYCLES: usize = 2;

// Erratum LCD-239: a transaction without a command and without dummy cycles
// needs more than two LCD_CLK cycles before its data.
const _: () = assert!(SETUP_CYCLES * CLOCK_CYCLES_PER_BUS_CYCLE > 2);

/// The configuration of the [`I8080`] driver for an [`I8080Bus`] with a write
/// clock of `frequency`: esp-hal's defaults, with [`SETUP_CYCLES`] setup
/// cycles.
pub fn configuration(frequency: Rate) -> Config {
    Config::default()
        .with_frequency(frequency)
        .with_setup_cycles(SETUP_CYCLES)
}

/// Bytes per transfer of data that has to be staged in internal SRAM.
const STAGING_SIZE: usize = 256;

/// Internal data RAM of the ESP32-S3 (esp-hal's `DRAM` region), which DMA
/// reads without cache maintenance.
const INTERNAL_DATA_RAM: core::ops::Range<usize> = 0x3FC8_8000..0x3FD0_0000;

/// Whether the `length` bytes at `start` lie in internal data RAM.
fn is_internal(start: usize, length: usize) -> bool {
    INTERNAL_DATA_RAM.contains(&start)
        && start
            .checked_add(length)
            .is_some_and(|end| end <= INTERNAL_DATA_RAM.end)
}

/// Where a slice is, for the DMA.
enum Region {
    /// Internal SRAM: DMA reads it directly.
    Internal,
    /// PSRAM, aligned to the cache line: DMA reads it after a write-back.
    Psram,
    /// Memory that the DMA cannot read directly: copy it first.
    Staged,
}

fn region(data: &[u8]) -> Region {
    if is_internal(data.as_ptr() as usize, data.len()) {
        return Region::Internal;
    }
    // Outside internal RAM, the checked constructor accepts only the mapped
    // PSRAM, with its address and length aligned to the data cache line.
    match DmaAlignedRef::new(data) {
        Ok(aligned) => {
            // Push the CPU's writes out of the cache before the DMA reads PSRAM.
            aligned.writeback();
            Region::Psram
        }
        Err(_) => Region::Staged,
    }
}

/// A DMA transmit buffer over borrowed data: the caller waits for the
/// transfer before the borrow ends.
struct BorrowedTransmitBuffer<'a> {
    descriptors: &'a mut [DmaDescriptor],
    /// Whether the data is in PSRAM, already written back from the cache.
    psram: bool,
    /// The data that the descriptors point to, borrowed for the transfer.
    data: PhantomData<&'a [u8]>,
}

impl<'a> BorrowedTransmitBuffer<'a> {
    /// A transfer of no data, for a command without parameters. The LCD_CAM
    /// always runs a DMA data phase, and the DMA rejects a descriptor whose
    /// buffer address is not real memory, such as an empty slice's dangling
    /// pointer. As esp-hal's own `DmaTxBuf` does for an empty write, the
    /// descriptor points at `target`, in internal SRAM, with a non-zero size
    /// and a length of zero.
    fn empty(descriptors: &'a mut [DmaDescriptor], target: &'a [u8; 4]) -> Option<Self> {
        let descriptor = descriptors.first_mut()?;
        descriptor.buffer = target.as_ptr().cast_mut();
        descriptor.set_size(target.len());
        descriptor.set_length(0);
        descriptor.next = core::ptr::null_mut();
        Some(Self {
            descriptors: core::slice::from_mut(descriptor),
            psram: false,
            data: PhantomData,
        })
    }

    /// Links enough of `descriptors` to cover `data`, which must not be
    /// empty, and which must be in internal SRAM or, with `psram`, in written
    /// back PSRAM; `None` if there are too few descriptors.
    fn new(descriptors: &'a mut [DmaDescriptor], data: &'a [u8], psram: bool) -> Option<Self> {
        if data.is_empty() {
            return None;
        }
        let count = data.len().div_ceil(DESCRIPTOR_CHUNK_SIZE);
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
        Some(Self {
            descriptors,
            psram,
            data: PhantomData,
        })
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
        Preparation {
            start: self.descriptors.as_mut_ptr(),
            accesses_psram: self.psram,
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
    /// The DMA descriptors are not in internal SRAM, where the DMA reads them.
    DescriptorsOutsideInternalMemory,
}

/// The 8080 bus: esp-hal's [`I8080`] driver and the DMA descriptors that its
/// transfers use.
pub struct I8080Bus {
    i8080: Option<I8080<'static, Blocking>>,
    descriptors: &'static mut [DmaDescriptor],
}

impl I8080Bus {
    /// Takes an [`I8080`] built with [`configuration`] and descriptors, which
    /// must be in internal SRAM. They bound one transfer to
    /// `descriptors.len() * DESCRIPTOR_CHUNK_SIZE` bytes; longer writes are
    /// split into several transfers.
    pub fn new(
        i8080: I8080<'static, Blocking>,
        descriptors: &'static mut [DmaDescriptor],
    ) -> Result<Self, I8080Error> {
        if descriptors.is_empty()
            || !is_internal(descriptors.as_ptr() as usize, size_of_val(descriptors))
        {
            return Err(I8080Error::DescriptorsOutsideInternalMemory);
        }
        Ok(Self {
            i8080: Some(i8080),
            descriptors,
        })
    }

    fn capacity(&self) -> usize {
        self.descriptors.len() * DESCRIPTOR_CHUNK_SIZE
    }

    /// Sends `data`, which is empty, in internal SRAM or, with `psram`, in
    /// written-back PSRAM.
    fn transfer(
        &mut self,
        command: Command<u8>,
        data: &[u8],
        psram: bool,
    ) -> Result<(), I8080Error> {
        // In internal SRAM: the stack of this call outlives the transfer.
        let empty_target = [0; 4];
        let buffer = if data.is_empty() {
            BorrowedTransmitBuffer::empty(self.descriptors, &empty_target)
        } else {
            BorrowedTransmitBuffer::new(self.descriptors, data, psram)
        }
        .ok_or(I8080Error::Unavailable)?;
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
                _ => self.transfer(command, &[], false),
            };
        }
        let psram = match region(data) {
            Region::Internal => false,
            Region::Psram => true,
            Region::Staged => {
                let mut staging = [0; STAGING_SIZE];
                for chunk in data.chunks(STAGING_SIZE) {
                    staging[..chunk.len()].copy_from_slice(chunk);
                    self.transfer(command, &staging[..chunk.len()], false)?;
                    command = Command::None;
                }
                return Ok(());
            }
        };
        for chunk in data.chunks(self.capacity()) {
            self.transfer(command, chunk, psram)?;
            command = Command::None;
        }
        Ok(())
    }
}
