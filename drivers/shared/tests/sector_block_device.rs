#![cfg(target_os = "linux")]
//! LittleFS on a sector block device, as the firmware mounts an SD card.
extern crate critical_section;
testing::instantiate_global_allocator!();

use std::sync::atomic::{AtomicUsize, Ordering};

use drivers_shared::devices::{SECTOR_SIZE, SectorBlockDevice, SectorStorage};
use file_system::{IoErrorKind, Result};

/// Sectors in memory that count the sectors written.
struct MemoryStorage {
    data: Vec<u8>,
    writes: &'static AtomicUsize,
}

impl SectorStorage for MemoryStorage {
    fn sector_count(&self) -> u32 {
        (self.data.len() / SECTOR_SIZE) as u32
    }

    fn read_sector(&mut self, index: u32, sector: &mut [u8; SECTOR_SIZE]) -> Result<()> {
        let start = index as usize * SECTOR_SIZE;
        let source = self.data.get(start..start + SECTOR_SIZE);
        sector.copy_from_slice(source.ok_or(IoErrorKind::InvalidArgument)?);
        Ok(())
    }

    fn write_sector(&mut self, index: u32, sector: &[u8; SECTOR_SIZE]) -> Result<()> {
        self.writes.fetch_add(1, Ordering::SeqCst);
        let start = index as usize * SECTOR_SIZE;
        self.data[start..start + SECTOR_SIZE].copy_from_slice(sector);
        Ok(())
    }
}

#[test]
fn littlefs_mounts_a_formatted_card_and_never_formats_another() {
    let writes: &'static AtomicUsize = Box::leak(Box::new(AtomicUsize::new(0)));
    let mut data = vec![0; 2048 * SECTOR_SIZE];
    // The start of a card from a computer: a boot sector with its signature.
    data[..15].copy_from_slice(b"\xEB\x58\x90MSDOS5.0\0\x02\x08\x20");
    data[510] = 0x55;
    data[511] = 0xAA;
    let device: &'static SectorBlockDevice<MemoryStorage> =
        Box::leak(Box::new(SectorBlockDevice::new(MemoryStorage {
            data,
            writes,
        })));
    let options = little_fs::Options::new(SECTOR_SIZE);

    // Mounting a card without LittleFS fails and writes nothing.
    assert!(little_fs::FileSystem::new(device, options).is_err());
    assert_eq!(writes.load(Ordering::SeqCst), 0);

    // An explicit format works with 4096-byte blocks and the no-op erase.
    drop(little_fs::FileSystem::new_format(device, options).unwrap());
    assert!(writes.load(Ordering::SeqCst) > 0);
    drop(little_fs::FileSystem::new(device, options).unwrap());
}
