//! A byte-addressed block device over storage that transfers whole 512-byte
//! sectors, such as an SD card.
//!
//! [`SectorBlockDevice`] implements the Core [`DirectBlockDevice`] trait. It
//! accepts reads and writes at any byte position and length, and splits each of
//! them into sector transfers. A write that does not cover a whole sector reads
//! the sector first and writes it back with the new bytes (read-modify-write).
//!
//! - **Bounded memory.** The device holds one 512-byte sector buffer. No
//!   transfer allocates memory, whatever its length.
//! - **Blocking calls.** Every operation finishes before it returns, as in the
//!   flash partition device of the firmware. Core's LittleFS calls the device
//!   from C callbacks that cannot suspend.
//! - **Exclusive access.** One operation runs at a time. A second caller gets
//!   [`IoErrorKind::ResourceBusy`] instead of waiting, and no lock disables
//!   interrupts while a sector transfers.
//! - **Erase.** An SD card erases internally, so the `ERASE` command succeeds
//!   without touching the card. The contents of an erased range are
//!   unspecified: they stay as they were until the next write.

use core::task::{Context, Poll};

use file_system::{
    ControlCommand, ControlCommandIdentifier, DirectBaseOperations, DirectBlockDevice, IoErrorKind,
    MountOperations, Result, Size,
    block_device::{ERASE, GET_BLOCK_COUNT, GET_BLOCK_SIZE},
};
use shared::AnyByLayout;
use synchronization::{blocking_mutex::raw::CriticalSectionRawMutex, mutex::Mutex};

/// The size of a sector in bytes. It is the block size that the device
/// reports.
pub const SECTOR_SIZE: usize = 512;

/// Storage that transfers one whole sector at a time.
///
/// Both transfer functions are blocking. An implementation reports a failed
/// transfer as an [`IoErrorKind`]; the device passes it to its caller.
pub trait SectorStorage: Send {
    /// The number of sectors of the storage.
    fn sector_count(&self) -> u32;

    /// Reads the sector `index` into `sector`.
    fn read_sector(&mut self, index: u32, sector: &mut [u8; SECTOR_SIZE]) -> Result<()>;

    /// Writes `sector` to the sector `index` and waits until the storage has
    /// accepted it.
    fn write_sector(&mut self, index: u32, sector: &[u8; SECTOR_SIZE]) -> Result<()>;
}

struct State<S> {
    storage: S,
    /// The sector of a partial transfer, kept in the device so that no
    /// operation allocates.
    scratch: [u8; SECTOR_SIZE],
}

/// A byte-addressed block device over [`SectorStorage`].
pub struct SectorBlockDevice<S: SectorStorage> {
    sector_count: u32,
    state: Mutex<CriticalSectionRawMutex, State<S>>,
}

impl<S: SectorStorage> SectorBlockDevice<S> {
    /// Wraps `storage`. The sector count is read once, here.
    pub fn new(storage: S) -> Self {
        Self {
            sector_count: storage.sector_count(),
            state: Mutex::new(State {
                storage,
                scratch: [0; SECTOR_SIZE],
            }),
        }
    }

    /// The capacity in bytes.
    pub fn capacity(&self) -> Size {
        Size::from(self.sector_count) * SECTOR_SIZE as Size
    }

    /// Reads at `position`, up to the end of the storage. Returns the number
    /// of bytes read.
    fn read_bytes(&self, buffer: &mut [u8], position: Size) -> Result<usize> {
        let capacity = self.capacity();
        if position > capacity {
            return Err(IoErrorKind::InvalidArgument);
        }
        // Compare in `Size`: on a 32-bit target, the bytes left on a card of
        // 4 GiB or more do not fit in `usize`.
        let length = (buffer.len() as Size).min(capacity - position) as usize;
        let buffer = &mut buffer[..length];

        let mut state = self
            .state
            .try_lock()
            .map_err(|_| IoErrorKind::ResourceBusy)?;
        let State { storage, scratch } = &mut *state;

        let mut done = 0;
        while done < length {
            let (sector, offset) = locate(position + done as Size);
            let count = (SECTOR_SIZE - offset).min(length - done);
            if count == SECTOR_SIZE {
                let target: &mut [u8; SECTOR_SIZE] =
                    (&mut buffer[done..done + SECTOR_SIZE]).try_into().unwrap();
                storage.read_sector(sector, target)?;
            } else {
                storage.read_sector(sector, scratch)?;
                buffer[done..done + count].copy_from_slice(&scratch[offset..offset + count]);
            }
            done += count;
        }
        Ok(length)
    }

    /// Writes all of `buffer` at `position`. The whole range must lie inside
    /// the storage.
    fn write_bytes(&self, buffer: &[u8], position: Size) -> Result<usize> {
        let capacity = self.capacity();
        if position > capacity || buffer.len() as Size > capacity - position {
            return Err(IoErrorKind::InvalidArgument);
        }

        let mut state = self
            .state
            .try_lock()
            .map_err(|_| IoErrorKind::ResourceBusy)?;
        let State { storage, scratch } = &mut *state;

        let mut done = 0;
        while done < buffer.len() {
            let (sector, offset) = locate(position + done as Size);
            let count = (SECTOR_SIZE - offset).min(buffer.len() - done);
            if count == SECTOR_SIZE {
                let source: &[u8; SECTOR_SIZE] =
                    (&buffer[done..done + SECTOR_SIZE]).try_into().unwrap();
                storage.write_sector(sector, source)?;
            } else {
                storage.read_sector(sector, scratch)?;
                scratch[offset..offset + count].copy_from_slice(&buffer[done..done + count]);
                storage.write_sector(sector, scratch)?;
            }
            done += count;
        }
        Ok(buffer.len())
    }
}

/// The sector that holds `position`, and the offset of `position` in it.
fn locate(position: Size) -> (u32, usize) {
    (
        (position / SECTOR_SIZE as Size) as u32,
        (position % SECTOR_SIZE as Size) as usize,
    )
}

impl<S: SectorStorage> DirectBaseOperations for SectorBlockDevice<S> {
    fn read(&self, _: &mut Context<'_>, buffer: &mut [u8], position: Size) -> Poll<Result<usize>> {
        file_system::ready(|| self.read_bytes(buffer, position))
    }

    fn write(&self, _: &mut Context<'_>, buffer: &[u8], position: Size) -> Poll<Result<usize>> {
        file_system::ready(|| self.write_bytes(buffer, position))
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
                *GET_BLOCK_SIZE::cast_output(output)? = SECTOR_SIZE as u32;
                Ok(())
            }
            GET_BLOCK_COUNT::IDENTIFIER => {
                *GET_BLOCK_COUNT::cast_output(output)? = self.sector_count;
                Ok(())
            }
            ERASE::IDENTIFIER => {
                let range = ERASE::cast_input(input)?;
                if range.position > self.capacity() || range.size > self.capacity() - range.position
                {
                    return Err(IoErrorKind::InvalidArgument);
                }
                Ok(())
            }
            _ => Err(IoErrorKind::Unsupported),
        })
    }
}

impl<S: SectorStorage> MountOperations for SectorBlockDevice<S> {}
impl<S: SectorStorage> DirectBlockDevice for SectorBlockDevice<S> {}

#[cfg(test)]
mod tests {
    use alloc::{boxed::Box, vec, vec::Vec};
    use core::sync::atomic::{AtomicUsize, Ordering};

    use super::*;

    /// Sectors in memory. It counts the transfers and can fail on demand.
    struct MemoryStorage {
        data: Vec<u8>,
        reads: &'static AtomicUsize,
        writes: &'static AtomicUsize,
        fail_sector: Option<u32>,
    }

    impl MemoryStorage {
        fn new(sectors: u32) -> Self {
            Self {
                data: vec![0; sectors as usize * SECTOR_SIZE],
                reads: Box::leak(Box::new(AtomicUsize::new(0))),
                writes: Box::leak(Box::new(AtomicUsize::new(0))),
                fail_sector: None,
            }
        }
    }

    impl SectorStorage for MemoryStorage {
        fn sector_count(&self) -> u32 {
            (self.data.len() / SECTOR_SIZE) as u32
        }

        fn read_sector(&mut self, index: u32, sector: &mut [u8; SECTOR_SIZE]) -> Result<()> {
            if self.fail_sector == Some(index) {
                return Err(IoErrorKind::InputOutput);
            }
            self.reads.fetch_add(1, Ordering::SeqCst);
            let start = index as usize * SECTOR_SIZE;
            sector.copy_from_slice(&self.data[start..start + SECTOR_SIZE]);
            Ok(())
        }

        fn write_sector(&mut self, index: u32, sector: &[u8; SECTOR_SIZE]) -> Result<()> {
            if self.fail_sector == Some(index) {
                return Err(IoErrorKind::InputOutput);
            }
            self.writes.fetch_add(1, Ordering::SeqCst);
            let start = index as usize * SECTOR_SIZE;
            self.data[start..start + SECTOR_SIZE].copy_from_slice(sector);
            Ok(())
        }
    }

    fn pattern(length: usize, seed: u8) -> Vec<u8> {
        (0..length)
            .map(|index| (index as u8).wrapping_mul(31).wrapping_add(seed))
            .collect()
    }

    #[test]
    fn whole_sector_write_does_not_read_first() {
        let storage = MemoryStorage::new(8);
        let (reads, writes) = (storage.reads, storage.writes);
        let device = SectorBlockDevice::new(storage);
        let data = pattern(2 * SECTOR_SIZE, 1);

        device.write_sync(&data, SECTOR_SIZE as Size).unwrap();

        assert_eq!(reads.load(Ordering::SeqCst), 0);
        assert_eq!(writes.load(Ordering::SeqCst), 2);
        let mut back = vec![0; data.len()];
        assert_eq!(
            device.read_sync(&mut back, SECTOR_SIZE as Size),
            Ok(back.len())
        );
        assert_eq!(back, data);
    }

    #[test]
    fn partial_write_keeps_the_rest_of_the_sector() {
        let device = SectorBlockDevice::new(MemoryStorage::new(4));
        let first = pattern(2 * SECTOR_SIZE, 7);
        device.write_sync(&first, 0).unwrap();

        // Crosses the sector boundary at 512 and starts and ends inside sectors.
        let patch = pattern(100, 200);
        device.write_sync(&patch, 480).unwrap();

        let mut expected = first.clone();
        expected[480..580].copy_from_slice(&patch);
        let mut back = vec![0; 2 * SECTOR_SIZE];
        device.read_sync(&mut back, 0).unwrap();
        assert_eq!(back, expected);
    }

    #[test]
    fn unaligned_read_spans_sectors() {
        let device = SectorBlockDevice::new(MemoryStorage::new(4));
        let data = pattern(4 * SECTOR_SIZE, 3);
        device.write_sync(&data, 0).unwrap();

        let mut back = vec![0; 1300];
        assert_eq!(device.read_sync(&mut back, 700), Ok(1300));
        assert_eq!(back, &data[700..2000]);
    }

    #[test]
    fn read_stops_at_the_end_of_the_storage() {
        let device = SectorBlockDevice::new(MemoryStorage::new(2));
        let capacity = device.capacity();
        assert_eq!(capacity, 1024);

        let mut back = [0u8; 64];
        assert_eq!(device.read_sync(&mut back, capacity - 10), Ok(10));
        assert_eq!(device.read_sync(&mut back, capacity), Ok(0));
        assert_eq!(
            device.read_sync(&mut back, capacity + 1),
            Err(IoErrorKind::InvalidArgument)
        );
    }

    /// Sectors that read as zeros and accept no write: a large card without
    /// the memory for it.
    struct EmptyStorage(u32);

    impl SectorStorage for EmptyStorage {
        fn sector_count(&self) -> u32 {
            self.0
        }

        fn read_sector(&mut self, _: u32, sector: &mut [u8; SECTOR_SIZE]) -> Result<()> {
            sector.fill(0);
            Ok(())
        }

        fn write_sector(&mut self, _: u32, _: &[u8; SECTOR_SIZE]) -> Result<()> {
            Err(IoErrorKind::PermissionDenied)
        }
    }

    #[test]
    fn read_on_a_card_larger_than_4_gibibytes_is_complete() {
        // 8 GiB: the bytes left after a position below 4 GiB exceed `u32`.
        let device = SectorBlockDevice::new(EmptyStorage(16 * 1024 * 1024));
        assert_eq!(device.capacity(), 8 * 1024 * 1024 * 1024);

        let mut back = [0xFFu8; 1000];
        assert_eq!(device.read_sync(&mut back, 0), Ok(back.len()));
        assert_eq!(device.read_sync(&mut back, 700), Ok(back.len()));
        assert_eq!(device.read_sync(&mut back, device.capacity() - 10), Ok(10));
        assert_eq!(back[..10], [0; 10]);
    }

    #[test]
    fn write_beyond_the_end_changes_nothing() {
        let storage = MemoryStorage::new(2);
        let writes = storage.writes;
        let device = SectorBlockDevice::new(storage);

        let data = [0xAA; 64];
        assert_eq!(
            device.write_sync(&data, device.capacity() - 10),
            Err(IoErrorKind::InvalidArgument)
        );
        assert_eq!(
            device.write_sync(&data, device.capacity() + 1),
            Err(IoErrorKind::InvalidArgument)
        );
        assert_eq!(writes.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn storage_errors_reach_the_caller() {
        let mut storage = MemoryStorage::new(4);
        storage.fail_sector = Some(2);
        let device = SectorBlockDevice::new(storage);

        let mut back = [0u8; 16];
        assert_eq!(
            device.read_sync(&mut back, 2 * SECTOR_SIZE as Size),
            Err(IoErrorKind::InputOutput)
        );
        assert_eq!(
            device.write_sync(&back, 2 * SECTOR_SIZE as Size),
            Err(IoErrorKind::InputOutput)
        );
        // Another sector still works.
        assert_eq!(device.write_sync(&back, 0), Ok(16));
    }

    #[test]
    fn reports_geometry() {
        let device = SectorBlockDevice::new(MemoryStorage::new(10));
        assert_eq!(device.get_block_size(), Ok(SECTOR_SIZE as u32));
        assert_eq!(device.get_block_count(), Ok(10));
    }

    #[test]
    fn erase_keeps_the_data_and_checks_the_range() {
        let device = SectorBlockDevice::new(MemoryStorage::new(2));
        let data = pattern(SECTOR_SIZE, 9);
        device.write_sync(&data, 0).unwrap();

        assert_eq!(device.erase(0, SECTOR_SIZE as Size), Ok(()));
        assert_eq!(
            device.erase(0, device.capacity() + 1),
            Err(IoErrorKind::InvalidArgument)
        );

        let mut back = vec![0; SECTOR_SIZE];
        device.read_sync(&mut back, 0).unwrap();
        assert_eq!(back, data);
    }

    #[test]
    fn unknown_commands_are_unsupported() {
        let device = SectorBlockDevice::new(MemoryStorage::new(1));
        let unknown = ControlCommandIdentifier::new_read::<(), u32>(b'Z', 99);
        assert_eq!(
            device.control_sync(unknown, AnyByLayout::NONE, AnyByLayout::NONE),
            Err(IoErrorKind::Unsupported)
        );
    }
}
