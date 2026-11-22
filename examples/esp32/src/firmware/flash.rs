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

// `START` and `LENGTH` come from the LittleFS partition of the partition table
// of the chip (`partitions_4mb.csv` or `partitions_16mb.csv`), read by `build.rs`.
include!(concat!(env!("OUT_DIR"), "/storage_partition.rs"));
const BLOCK: u32 = FlashStorage::ERASE_SIZE as u32;

pub(super) struct LittleFsPartition(Mutex<CriticalSectionRawMutex, RefCell<FlashStorage<'static>>>);

impl LittleFsPartition {
    pub(super) fn new(storage: FlashStorage<'static>) -> Self {
        Self(Mutex::new(RefCell::new(storage)))
    }
}

fn address(position: Size, length: Size) -> Result<u32> {
    if position > LENGTH.into() || length > Size::from(LENGTH) - position {
        return Err(IoErrorKind::InvalidArgument);
    }
    Ok(START + position as u32)
}

impl DirectBaseOperations for LittleFsPartition {
    fn read(&self, _: &mut Context<'_>, buffer: &mut [u8], position: Size) -> Poll<Result<usize>> {
        file_system::ready(|| {
            let address = address(position, buffer.len() as Size)?;
            self.0
                .lock(|flash| ReadNorFlash::read(&mut *flash.borrow_mut(), address, buffer))
                .map_err(|_| IoErrorKind::InputOutput)?;
            Ok(buffer.len())
        })
    }

    fn write(&self, _: &mut Context<'_>, buffer: &[u8], position: Size) -> Poll<Result<usize>> {
        file_system::ready(|| {
            let address = address(position, buffer.len() as Size)?;
            self.0
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
                *GET_BLOCK_COUNT::cast_output(output)? = LENGTH / BLOCK;
                Ok(())
            }
            ERASE::IDENTIFIER => {
                let range = ERASE::cast_input(input)?;
                let start = address(range.position, range.size)?;
                let end = start + range.size as u32;
                self.0
                    .lock(|flash| NorFlash::erase(&mut *flash.borrow_mut(), start, end))
                    .map_err(|_| IoErrorKind::InputOutput)
            }
            _ => Err(IoErrorKind::Unsupported),
        })
    }
}

impl MountOperations for LittleFsPartition {}
impl DirectBlockDevice for LittleFsPartition {}
