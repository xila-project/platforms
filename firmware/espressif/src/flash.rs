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
            self.flash
                .lock(|flash| ReadNorFlash::read(&mut *flash.borrow_mut(), address, buffer))
                .map_err(|_| IoErrorKind::InputOutput)?;
            Ok(buffer.len())
        })
    }

    fn write(&self, _: &mut Context<'_>, buffer: &[u8], position: Size) -> Poll<Result<usize>> {
        file_system::ready(|| {
            let address = self.address(position, buffer.len() as Size)?;
            self.flash
                .lock(|flash| NorFlash::write(&mut *flash.borrow_mut(), address, buffer))
                .map_err(|_| IoErrorKind::InputOutput)?;
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
