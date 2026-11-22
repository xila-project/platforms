//! Executable memory for the native loader on Espressif RISC-V chips whose internal
//! SRAM has one address for instructions and data.
//!
//! # Supported chips
//!
//! - **ESP32-C5, ESP32-C6, ESP32-C61 and ESP32-H2**: the HP SRAM is unified and not
//!   cached, so finalization only needs `fence` and `fence.i`.
//! - **ESP32-P4**: the L2 memory is unified too, but the CPU reaches it through its L1
//!   caches: finalization writes the L1 data cache back and invalidates both cores'
//!   L1 instruction caches over the image, with the same ROM routines esp-hal uses.
//!
//! The other chips have no implementation, so firmware cannot register the native
//! handler on them:
//!
//! - **ESP32, ESP32-S2 and ESP32-S3** (Xtensa): the instruction and data buses see the
//!   same SRAM, and PSRAM through the MMU, at different addresses, whereas the
//!   loader's [`native_loader::ExecutableMemory`] contract needs one address range
//!   for both (and the data bus cannot store bytes through instruction addresses).
//!   Running Xtensa images on the coroutine backend is not available either
//!   (xila-project/core#648).
//! - **ESP32-C2 and ESP32-C3**: their SRAM also has separate instruction and data bus
//!   addresses. They stay unsupported until an on-target test shows whether data
//!   loads through instruction-bus addresses are reliable.
//! - **ESP32-S31**: not evaluated yet.
//!
//! # Placement and memory protection
//!
//! With its default `ESP_HAL_CONFIG_ENABLE_PMP`, esp-hal locks Physical Memory
//! Protection entries at start-up: `.rwtext` becomes read and execute, and the
//! static data (`.data` to `.noinit`, which holds `static` heaps) and the main stack
//! become read and write without execute. Locked entries also bind machine mode and
//! cannot be changed afterwards, so images can only live in RAM outside these
//! ranges, which no entry covers. The bootloader's memory, which esp-hal links as
//! `.dram2_uninit` after the stack, is such a range:
//!
//! ```ignore
//! #[unsafe(link_section = ".dram2_uninit")]
//! static mut EXECUTABLE_ARENA: [MaybeUninit<u8>; 48 * 1024] = [MaybeUninit::uninit(); 48 * 1024];
//! ```
//!
//! [`ExecutableMemory::initialize`] refuses an arena that overlaps a protected range.
//! The remaining memory has no hardware permissions, so the backend applies none:
//! images are trusted (they run only through the developer `native-dev` handler).

use core::{alloc::Layout, mem::MaybeUninit, ptr::NonNull};

use memory::{CapabilityFlags, ManagerTrait, RegionDescriptor, RegionManager};
use native_loader::{Error, Region};

const DESCRIPTOR: RegionDescriptor = RegionDescriptor::new(CapabilityFlags::Executable);

/// Executable memory taken from a dedicated arena of internal SRAM.
pub struct ExecutableMemory {
    arena: RegionManager<1>,
}

impl Default for ExecutableMemory {
    fn default() -> Self {
        Self::new()
    }
}

/// The ranges esp-hal protects with locked PMP entries, as `[start, end)`.
fn protected_ranges() -> [(usize, usize); 3] {
    unsafe extern "C" {
        static _rwtext_start: u8;
        static _rwtext_end: u8;
        static _data_start: u8;
        static _noinit_end: u8;
        static _stack_end_cpu0: u8;
        static _stack_start_cpu0: u8;
    }
    let address = |symbol: *const u8| symbol as usize;
    [
        (
            address(&raw const _rwtext_start),
            address(&raw const _rwtext_end),
        ),
        (
            address(&raw const _data_start),
            address(&raw const _noinit_end),
        ),
        (
            address(&raw const _stack_end_cpu0),
            address(&raw const _stack_start_cpu0),
        ),
    ]
}

/// Whether `[start, start + size)` overlaps one of `ranges`.
fn overlaps(start: usize, size: usize, ranges: &[(usize, usize)]) -> bool {
    let Some(end) = start.checked_add(size) else {
        return true;
    };
    ranges
        .iter()
        .any(|&(range_start, range_end)| start < range_end && range_start < end)
}

impl ExecutableMemory {
    /// Creates an empty backend: allocations fail until [`Self::initialize`].
    pub const fn new() -> Self {
        Self {
            arena: RegionManager::new([DESCRIPTOR], None),
        }
    }

    /// Gives the backend the arena `[start, start + size)`.
    ///
    /// Returns `false`, and leaves the backend empty, if the arena overlaps a range
    /// that esp-hal protects against execution or writing (see the
    /// [module documentation](self)), is too small, or the backend already has one.
    ///
    /// # Safety
    ///
    /// The range must be internal SRAM of the selected chip, which the CPU executes
    /// at the addresses it writes, reserved exclusively for this backend for the
    /// lifetime of the firmware.
    pub unsafe fn initialize(&self, start: *mut u8, size: usize) -> bool {
        if self.arena.is_region_initialized(0)
            || overlaps(start as usize, size, &protected_ranges())
        {
            return false;
        }
        unsafe { self.arena.init_region(0, start, size) }
    }

    /// Gives the backend a static arena; see [`Self::initialize`].
    ///
    /// # Safety
    ///
    /// As for [`Self::initialize`].
    pub unsafe fn initialize_from_slice(&self, slice: &'static mut [MaybeUninit<u8>]) -> bool {
        unsafe { self.initialize(slice.as_mut_ptr().cast(), slice.len()) }
    }

    /// The bytes currently held by loaded images.
    pub fn get_used(&self) -> usize {
        self.arena.get_used()
    }

    /// The bytes still available in the arena.
    pub fn get_free(&self) -> usize {
        self.arena.get_free()
    }
}

/// Make the instructions written to `[start, start + size)` visible to instruction
/// fetch.
fn synchronize_instruction_fetch(start: usize, size: usize) {
    #[cfg(feature = "esp32p4")]
    {
        const CACHE_MAP_L1_ICACHE_0: u32 = 1 << 0;
        const CACHE_MAP_L1_ICACHE_1: u32 = 1 << 1;
        const CACHE_MAP_L1_DCACHE: u32 = 1 << 4;

        unsafe extern "C" {
            fn Cache_WriteBack_Addr(bus: u32, address: u32, size: u32);
            fn Cache_Invalidate_Addr(bus: u32, address: u32, size: u32);
        }
        // SAFETY: the ROM routines only maintain the caches over the given range,
        // which is mapped internal memory; neither changes its contents.
        unsafe {
            Cache_WriteBack_Addr(CACHE_MAP_L1_DCACHE, start as u32, size as u32);
            Cache_Invalidate_Addr(
                CACHE_MAP_L1_ICACHE_0 | CACHE_MAP_L1_ICACHE_1,
                start as u32,
                size as u32,
            );
        }
    }
    #[cfg(not(feature = "esp32p4"))]
    let _ = (start, size);

    // SAFETY: fences have no memory-safety preconditions.
    unsafe { core::arch::asm!("fence", "fence.i", options(nostack)) };
}

// SAFETY: the arena hands out exclusive blocks of the requested layout, which the CPU
// reads, writes and executes at the same addresses. Finalization keeps the block and
// its contents, and makes the writes visible to instruction fetch.
unsafe impl native_loader::ExecutableMemory for ExecutableMemory {
    fn allocate(&self, layout: Layout) -> Result<NonNull<u8>, Error> {
        if layout.size() == 0 {
            return Err(Error::AllocationFailed);
        }
        // SAFETY: the block is released through `free` with the same layout.
        unsafe { self.arena.allocate(CapabilityFlags::Executable, layout) }
            .ok_or(Error::AllocationFailed)
    }

    unsafe fn finish_writes(
        &self,
        allocation: NonNull<u8>,
        layout: Layout,
        regions: &mut dyn Iterator<Item = Region>,
    ) -> Result<(), Error> {
        for region in regions {
            let end = region
                .offset
                .checked_add(region.size)
                .ok_or(Error::FinalizationFailed)?;
            if end > layout.size() {
                return Err(Error::FinalizationFailed);
            }
        }
        synchronize_instruction_fetch(allocation.as_ptr() as usize, layout.size());
        Ok(())
    }

    unsafe fn free(&self, allocation: NonNull<u8>, layout: Layout) {
        // SAFETY: the caller passes a block of this backend, which nothing uses any more.
        unsafe { self.arena.deallocate(allocation, layout) };
    }
}
