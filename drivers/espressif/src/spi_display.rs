//! Asynchronous 4-wire SPI display bus over esp-hal's HSPI/VSPI DMA driver
//! (feature `spi-display`, ESP32).
//!
//! [`SpiDisplayBus`] implements [`AsynchronousDisplayBus`] so that:
//!
//! - Panel commands (`0x2A`, `0x2B`, `0x2C`, and initialisation sequences) go
//!   directly through the 64-byte hardware SPI FIFO without DMA resets or
//!   staging copies.
//! - Large, 4-byte-aligned pixel buffers in internal SRAM stream directly via
//!   non-blocking `SpiDma::write_buffer` with chip select held across `RAMWR`
//!   and the pixel payload, allowing LVGL to render into a second draw buffer
//!   while DMA clocks out the first.
//! - Small (`<= 64` bytes), unaligned, or non-DMA-capable slices fall back to
//!   synchronous FIFO transfers cleanly.

use drivers_shared::devices::display::AsynchronousDisplayBus;
use esp_hal::{
    Blocking,
    dma::{BurstConfig, DmaDescriptor, DmaTxBuffer, Preparation},
    gpio::Output,
    spi::{
        Error as SpiError,
        master::{SpiDma, SpiDmaTransfer},
    },
};

/// Bytes per DMA descriptor: at most 4095, kept a multiple of 4 for ESP32 PDMA.
pub const DESCRIPTOR_CHUNK_SIZE: usize = 4092;

/// Maximum bytes in a single ESP32 SPI DMA transfer (`esp-hal` limit).
pub const MAXIMUM_DMA_TRANSFER_SIZE: usize = 32_736;

/// Hardware SPI FIFO width in bytes on the ESP32.
const FIFO_SIZE: usize = 64;

/// Value to pass to `esp_hal::spi::master::Config::with_min_async_transfer_size`
/// so transfers up to the 64-byte FIFO bypass DMA setup completely.
pub const MINIMUM_DMA_TRANSFER_SIZE: usize = FIFO_SIZE + 1;

/// Internal data RAM of the ESP32 (esp-hal's `DRAM` range, covering both
/// primary DRAM and reclaimed DRAM2), which SPI PDMA reads directly.
const INTERNAL_DATA_RAM: core::ops::Range<usize> = 0x3FFA_E000..0x4000_0000;

const COLUMN_ADDRESS_SET: u8 = 0x2A;
const ROW_ADDRESS_SET: u8 = 0x2B;
const MEMORY_WRITE: u8 = 0x2C;

/// Whether `length` bytes starting at `start` lie wholly in internal DRAM.
fn is_internal(start: usize, length: usize) -> bool {
    INTERNAL_DATA_RAM.contains(&start)
        && start
            .checked_add(length)
            .is_some_and(|end| end <= INTERNAL_DATA_RAM.end)
}

/// Whether `data` meets ESP32 SPI TX PDMA region and 4-byte alignment rules.
fn is_dma_eligible(data: &[u8]) -> bool {
    let address = data.as_ptr() as usize;
    !data.is_empty()
        && address.is_multiple_of(4)
        && data.len().is_multiple_of(4)
        && is_internal(address, data.len())
}

/// A DMA transmit buffer over caller-owned internal SRAM memory that remains
/// valid and unmodified until the owning [`SpiDmaTransfer`] completes.
struct StaticTransmitBuffer {
    descriptors: &'static mut [DmaDescriptor],
    descriptor_count: usize,
}

impl StaticTransmitBuffer {
    /// Populates `descriptors` to stream `data`, which must satisfy
    /// [`is_dma_eligible`] and fit in `descriptors` and
    /// [`MAXIMUM_DMA_TRANSFER_SIZE`].
    fn new(
        descriptors: &'static mut [DmaDescriptor],
        data: &[u8],
    ) -> Result<Self, &'static mut [DmaDescriptor]> {
        if data.is_empty() || data.len() > MAXIMUM_DMA_TRANSFER_SIZE {
            return Err(descriptors);
        }
        let descriptor_count = data.len().div_ceil(DESCRIPTOR_CHUNK_SIZE);
        if descriptor_count > descriptors.len() {
            return Err(descriptors);
        }

        let mut next = core::ptr::null_mut();
        for (index, descriptor) in descriptors[..descriptor_count].iter_mut().enumerate().rev() {
            let start = index * DESCRIPTOR_CHUNK_SIZE;
            let chunk_length = (data.len() - start).min(DESCRIPTOR_CHUNK_SIZE);
            // The DMA only reads through this pointer.
            descriptor.buffer = data[start..].as_ptr().cast_mut();
            descriptor.set_size(chunk_length);
            descriptor.set_length(chunk_length);
            descriptor.next = next;
            next = descriptor;
        }

        Ok(Self {
            descriptors,
            descriptor_count,
        })
    }

    fn into_descriptors(self) -> &'static mut [DmaDescriptor] {
        self.descriptors
    }
}

// SAFETY: `SpiDisplayBus` starts a `StaticTransmitBuffer` transfer only on
// caller-guaranteed internal SRAM buffers that remain valid and unmodified
// until `wait_for_transfer` (or `Drop`) finishes the `SpiDmaTransfer`.
unsafe impl DmaTxBuffer for StaticTransmitBuffer {
    type View = Self;
    type Final = Self;

    fn prepare(&mut self) -> Preparation {
        let last = self.descriptor_count - 1;
        for (index, descriptor) in self.descriptors[..self.descriptor_count]
            .iter_mut()
            .enumerate()
        {
            descriptor.reset_for_tx(index == last);
        }
        Preparation {
            start: self.descriptors.as_mut_ptr(),
            burst_transfer: BurstConfig::default(),
            check_owner: None,
            auto_write_back: false,
        }
    }

    fn into_view(self) -> Self {
        self
    }

    fn from_view(view: Self) -> Self {
        view
    }
}

/// An SPI display bus error.
#[derive(Debug)]
pub enum SpiDisplayError {
    /// An `esp-hal` SPI or DMA error.
    Spi(SpiError),
    /// A previous operation left the peripheral unavailable.
    Unavailable,
    /// The DMA descriptors are empty, unaligned, or outside internal SRAM.
    InvalidDescriptors,
}

enum BusState {
    Idle(SpiDma<'static, Blocking>),
    Transferring(SpiDmaTransfer<'static, Blocking, StaticTransmitBuffer>),
    Unavailable,
}

/// 4-wire SPI display bus with FIFO command writes and non-blocking DMA pixel
/// transfers.
pub struct SpiDisplayBus {
    state: BusState,
    select: Output<'static>,
    command_data: Output<'static>,
    descriptors: Option<&'static mut [DmaDescriptor]>,
}

impl SpiDisplayBus {
    /// Wraps a configured [`SpiDma`] (built with
    /// `.with_min_async_transfer_size(MINIMUM_DMA_TRANSFER_SIZE)`), its chip
    /// select and data/command pins, and static DMA descriptors in internal
    /// SRAM.
    pub fn new(
        spi: SpiDma<'static, Blocking>,
        mut select: Output<'static>,
        command_data: Output<'static>,
        descriptors: &'static mut [DmaDescriptor],
    ) -> Result<Self, SpiDisplayError> {
        let address = descriptors.as_ptr() as usize;
        if descriptors.is_empty()
            || !address.is_multiple_of(4)
            || !is_internal(address, size_of_val(descriptors))
        {
            return Err(SpiDisplayError::InvalidDescriptors);
        }
        select.set_high();
        Ok(Self {
            state: BusState::Idle(spi),
            select,
            command_data,
            descriptors: Some(descriptors),
        })
    }

    fn capacity(&self) -> usize {
        self.descriptors.as_ref().map_or(0, |descriptors| {
            (descriptors.len() * DESCRIPTOR_CHUNK_SIZE).min(MAXIMUM_DMA_TRANSFER_SIZE)
        })
    }

    fn idle_spi(&mut self) -> Result<&mut SpiDma<'static, Blocking>, SpiDisplayError> {
        self.wait_for_transfer()?;
        match &mut self.state {
            BusState::Idle(spi) => Ok(spi),
            _ => Err(SpiDisplayError::Unavailable),
        }
    }

    fn write_fifo_chunks(
        spi: &mut SpiDma<'static, Blocking>,
        data: &[u8],
    ) -> Result<(), SpiDisplayError> {
        for chunk in data.chunks(FIFO_SIZE) {
            spi.write(chunk).map_err(SpiDisplayError::Spi)?;
        }
        Ok(())
    }

    fn write_command_fifo(&mut self, command: u8, arguments: &[u8]) -> Result<(), SpiDisplayError> {
        self.wait_for_transfer()?;
        let BusState::Idle(spi) = &mut self.state else {
            return Err(SpiDisplayError::Unavailable);
        };
        self.select.set_low();
        self.command_data.set_low();
        let result = spi
            .write(&[command])
            .map_err(SpiDisplayError::Spi)
            .and_then(|()| {
                if arguments.is_empty() {
                    Ok(())
                } else {
                    self.command_data.set_high();
                    Self::write_fifo_chunks(spi, arguments)
                }
            });
        self.select.set_high();
        result
    }

    /// Launches a single DMA transfer with chip select already low and
    /// data/command already high. Raises chip select again when the transfer
    /// cannot start.
    fn start_dma_slice(&mut self, data: &[u8]) -> Result<(), SpiDisplayError> {
        let result = self.try_start_dma_slice(data);
        if result.is_err() {
            self.select.set_high();
        }
        result
    }

    fn try_start_dma_slice(&mut self, data: &[u8]) -> Result<(), SpiDisplayError> {
        let descriptors = self
            .descriptors
            .take()
            .ok_or(SpiDisplayError::Unavailable)?;
        let buffer = match StaticTransmitBuffer::new(descriptors, data) {
            Ok(buffer) => buffer,
            Err(descriptors) => {
                self.descriptors = Some(descriptors);
                return Err(SpiDisplayError::Unavailable);
            }
        };
        let BusState::Idle(spi) = core::mem::replace(&mut self.state, BusState::Unavailable) else {
            self.descriptors = Some(buffer.into_descriptors());
            return Err(SpiDisplayError::Unavailable);
        };
        match spi.write_buffer(data.len(), buffer) {
            Ok(transfer) => {
                self.state = BusState::Transferring(transfer);
                Ok(())
            }
            Err((error, spi, buffer)) => {
                self.descriptors = Some(buffer.into_descriptors());
                self.state = BusState::Idle(spi);
                Err(SpiDisplayError::Spi(error))
            }
        }
    }

    fn write_pixels_synchronous(&mut self, data: &[u8]) -> Result<(), SpiDisplayError> {
        if data.is_empty() {
            return Ok(());
        }
        let capacity = self.capacity();
        if data.len() > FIFO_SIZE && capacity > 0 && is_dma_eligible(data) {
            for chunk in data.chunks(capacity) {
                self.start_dma_slice(chunk)?;
                // `wait_for_transfer` raises chip select, so re-assert for any
                // follow-up chunk.
                self.finish_transfer_keep_select()?;
            }
            Ok(())
        } else {
            let spi = self.idle_spi()?;
            Self::write_fifo_chunks(spi, data)
        }
    }

    fn finish_transfer_keep_select(&mut self) -> Result<(), SpiDisplayError> {
        match core::mem::replace(&mut self.state, BusState::Unavailable) {
            BusState::Idle(spi) => {
                self.state = BusState::Idle(spi);
                Ok(())
            }
            BusState::Transferring(transfer) => {
                let (spi, buffer) = transfer.wait();
                self.descriptors = Some(buffer.into_descriptors());
                self.state = BusState::Idle(spi);
                Ok(())
            }
            BusState::Unavailable => Err(SpiDisplayError::Unavailable),
        }
    }
}

impl Drop for SpiDisplayBus {
    fn drop(&mut self) {
        let _ = self.wait_for_transfer();
    }
}

impl AsynchronousDisplayBus for SpiDisplayBus {
    type Error = SpiDisplayError;

    fn send_command(&mut self, command: u8, arguments: &[u8]) -> Result<(), Self::Error> {
        self.write_command_fifo(command, arguments)
    }

    fn send_pixels(&mut self, data: &[u8]) -> Result<(), Self::Error> {
        if data.is_empty() {
            return Ok(());
        }
        self.wait_for_transfer()?;
        self.select.set_low();
        self.command_data.set_high();
        let result = self.write_pixels_synchronous(data);
        self.select.set_high();
        result
    }

    unsafe fn start_window_pixels(
        &mut self,
        x_start: u16,
        y_start: u16,
        x_end: u16,
        y_end: u16,
        data: &[u8],
    ) -> Result<bool, Self::Error> {
        self.wait_for_transfer()?;
        if data.is_empty() {
            return Ok(false);
        }

        let columns = [
            (x_start >> 8) as u8,
            x_start as u8,
            (x_end >> 8) as u8,
            x_end as u8,
        ];
        let rows = [
            (y_start >> 8) as u8,
            y_start as u8,
            (y_end >> 8) as u8,
            y_end as u8,
        ];
        self.write_command_fifo(COLUMN_ADDRESS_SET, &columns)?;
        self.write_command_fifo(ROW_ADDRESS_SET, &rows)?;

        // Keep chip select asserted across RAMWR and the pixel stream.
        let BusState::Idle(spi) = &mut self.state else {
            return Err(SpiDisplayError::Unavailable);
        };
        self.select.set_low();
        self.command_data.set_low();
        if let Err(error) = spi.write(&[MEMORY_WRITE]) {
            self.select.set_high();
            return Err(SpiDisplayError::Spi(error));
        }
        self.command_data.set_high();

        let capacity = self.capacity();
        if data.len() > FIFO_SIZE && data.len() <= capacity && is_dma_eligible(data) {
            self.start_dma_slice(data)?;
            Ok(true)
        } else {
            let result = self.write_pixels_synchronous(data);
            self.select.set_high();
            result.map(|()| false)
        }
    }

    fn wait_for_transfer(&mut self) -> Result<(), Self::Error> {
        match core::mem::replace(&mut self.state, BusState::Unavailable) {
            BusState::Idle(spi) => {
                self.state = BusState::Idle(spi);
                Ok(())
            }
            BusState::Transferring(transfer) => {
                let (spi, buffer) = transfer.wait();
                self.select.set_high();
                self.descriptors = Some(buffer.into_descriptors());
                self.state = BusState::Idle(spi);
                Ok(())
            }
            BusState::Unavailable => Err(SpiDisplayError::Unavailable),
        }
    }
}
