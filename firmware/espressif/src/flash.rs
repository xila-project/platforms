use core::{
    cell::RefCell,
    task::{Context, Poll},
};

use embedded_storage::nor_flash::{NorFlash, ReadNorFlash};
use esp_storage::FlashStorage;
use xila::{
    file_system::{
        self, ControlCommand, ControlCommandIdentifier, DirectBaseOperations, DirectBlockDevice,
        IoErrorKind, MountOperations, Result, Size,
        block_device::{ERASE, GET_BLOCK_COUNT, GET_BLOCK_SIZE},
    },
    shared::AnyByLayout,
    synchronization::blocking_mutex::{Mutex, raw::CriticalSectionRawMutex},
};

use crate::StoragePartition;

const BLOCK: u32 = FlashStorage::ERASE_SIZE as u32;

// The ROM flash routines may suspend the cache, making PSRAM unavailable.
// Keep the transfer buffer on the internal SRAM stack and copy outside the
// ROM call, even when the caller's buffer is already word-aligned.
const TRANSFER_SIZE: usize = super::CACHE_SIZE;

#[repr(align(4))]
struct TransferBuffer([u8; TRANSFER_SIZE]);

/// The board's storage partition as a block device.
pub(super) struct LittleFsPartition {
    flash: Mutex<CriticalSectionRawMutex, RefCell<FlashStorage<'static>>>,
    partition: StoragePartition,
}

impl LittleFsPartition {
    pub(super) fn new(storage: FlashStorage<'static>, partition: StoragePartition) -> Self {
        Self {
            flash: Mutex::new(RefCell::new(storage)),
            partition,
        }
    }

    fn address(&self, position: Size, length: Size) -> Result<u32> {
        let partition_length = Size::from(self.partition.length);
        if position > partition_length || length > partition_length - position {
            return Err(IoErrorKind::InvalidArgument);
        }
        Ok(self.partition.offset + position as u32)
    }
}

impl DirectBaseOperations for LittleFsPartition {
    fn read(&self, _: &mut Context<'_>, buffer: &mut [u8], position: Size) -> Poll<Result<usize>> {
        file_system::ready(|| {
            let address = self.address(position, buffer.len() as Size)?;
            let alignment = <FlashStorage<'_> as ReadNorFlash>::READ_SIZE;
            if !((address as usize).is_multiple_of(alignment)
                && buffer.len().is_multiple_of(alignment))
            {
                return Err(IoErrorKind::InputOutput);
            }
            self.flash.lock(|flash| -> Result<()> {
                let mut flash = flash.borrow_mut();
                let mut transfer = TransferBuffer([0; TRANSFER_SIZE]);
                for (index, chunk) in buffer.chunks_mut(TRANSFER_SIZE).enumerate() {
                    let offset = address + (index * TRANSFER_SIZE) as u32;
                    let scratch = &mut transfer.0[..chunk.len()];
                    ReadNorFlash::read(&mut *flash, offset, scratch)
                        .map_err(|_| IoErrorKind::InputOutput)?;
                    chunk.copy_from_slice(scratch);
                }
                Ok(())
            })?;
            Ok(buffer.len())
        })
    }

    fn write(&self, _: &mut Context<'_>, buffer: &[u8], position: Size) -> Poll<Result<usize>> {
        file_system::ready(|| {
            let address = self.address(position, buffer.len() as Size)?;
            let alignment = <FlashStorage<'_> as NorFlash>::WRITE_SIZE;
            if !((address as usize).is_multiple_of(alignment)
                && buffer.len().is_multiple_of(alignment))
            {
                return Err(IoErrorKind::InputOutput);
            }
            self.flash.lock(|flash| -> Result<()> {
                let mut flash = flash.borrow_mut();
                let mut transfer = TransferBuffer([0; TRANSFER_SIZE]);
                for (index, chunk) in buffer.chunks(TRANSFER_SIZE).enumerate() {
                    let offset = address + (index * TRANSFER_SIZE) as u32;
                    let scratch = &mut transfer.0[..chunk.len()];
                    scratch.copy_from_slice(chunk);
                    NorFlash::write(&mut *flash, offset, scratch)
                        .map_err(|_| IoErrorKind::InputOutput)?;
                }
                Ok(())
            })?;
            Ok(buffer.len())
        })
    }

    fn control(
        &self,
        _: &mut Context<'_>,
        command: ControlCommandIdentifier,
        input: &AnyByLayout,
        output: &mut AnyByLayout,
    ) -> Poll<Result<()>> {
        file_system::ready(|| match command {
            GET_BLOCK_SIZE::IDENTIFIER => {
                *GET_BLOCK_SIZE::cast_output(output)? = BLOCK;
                Ok(())
            }
            GET_BLOCK_COUNT::IDENTIFIER => {
                *GET_BLOCK_COUNT::cast_output(output)? = self.partition.length / BLOCK;
                Ok(())
            }
            ERASE::IDENTIFIER => {
                let range = ERASE::cast_input(input)?;
                let start = self.address(range.position, range.size)?;
                let end = start + range.size as u32;
                self.flash
                    .lock(|flash| NorFlash::erase(&mut *flash.borrow_mut(), start, end))
                    .map_err(|_| IoErrorKind::InputOutput)
            }
            _ => Err(IoErrorKind::Unsupported),
        })
    }
}

impl MountOperations for LittleFsPartition {}
impl DirectBlockDevice for LittleFsPartition {}
