//! Executable memory for the native loader on Espressif chips.
//!
//! # Supported chips
//!
//! ## Unified-bus RISC-V chips
//!
//! Instruction and data accesses use the same internal SRAM address, so
//! [`native_loader::ExecutableMemory::execution_base`] inherits the default
//! identity mapping:
//!
//! - **ESP32-C5, ESP32-C6, ESP32-C61 and ESP32-H2**: the HP SRAM is unified and not
//!   cached, so finalization only needs `fence` and `fence.i`.
//! - **ESP32-P4**: the L2 memory is unified too, but the CPU reaches it through its L1
//!   caches: finalization writes the L1 data cache back twice and invalidates both
//!   cores' L1 instruction caches over the image.
//!
//! ## Split-bus D/IRAM Xtensa chips
//!
//! Internal D/IRAM exposes the same physical SRAM at separate data-bus (DRAM) and
//! instruction-bus (IRAM) addresses separated by a constant offset. The loader writes
//! and relocates through the DRAM address, executes through
//! [`native_loader::ExecutableMemory::execution_base`] (`allocation + IRAM_OFFSET`),
//! and finalization issues `memw` and `isync`:
//!
//! - **ESP32-S3**: D/IRAM DRAM `0x3FC8_8000..0x3FCF_0000`, IRAM offset `+0x006F_0000`
//!   (`0x4037_8000..0x403E_0000`).
//! - **ESP32-S2**: D/IRAM DRAM `0x3FFB_0000..0x4000_0000`, IRAM offset `+0x0007_0000`
//!   (`0x4002_0000..0x4007_0000`).
//!
//! ## Unsupported chips
//!
//! The other chips have no implementation, so firmware cannot register the native
//! handler on them:
//!
//! - **ESP32**: SRAM1's instruction and data bus addresses run in reverse word
//!   order relative to each other, so a contiguous DRAM allocation does not map to
//!   a monotonic IRAM range.
//! - **ESP32-C2 and ESP32-C3**: their SRAM has separate instruction and data bus
//!   addresses, and RISC-V PC-relative addressing (`auipc`) from the instruction
//!   bus cannot reach data-bus addresses across the split segments.
//! - **ESP32-S31**: not evaluated yet.
//!
//! # Placement and memory protection
//!
//! On the RISC-V chips, with its default `ESP_HAL_CONFIG_ENABLE_PMP`, esp-hal locks
//! Physical Memory Protection entries at start-up: `.rwtext` becomes read and
//! execute, and the static data (`.data` to `.noinit`, which holds `static` heaps)
//! and the main stack become read and write without execute. Locked entries also
//! bind machine mode and cannot be changed afterwards, so images can only live in
//! RAM outside these ranges, which no entry covers. The bootloader's memory, which
//! esp-hal links as `.dram2_uninit` after the stack, is such a range:
//!
//! ```ignore
//! #[unsafe(link_section = ".dram2_uninit")]
//! static mut EXECUTABLE_ARENA: [MaybeUninit<u8>; 48 * 1024] = [MaybeUninit::uninit(); 48 * 1024];
//! ```
//!
//! [`ExecutableMemory::initialize`] refuses an arena that overlaps a protected range
//! on RISC-V, or one that falls outside internal D/IRAM or aliases `.rwtext` on the
//! ESP32-S2 and ESP32-S3 (where `.dram2_uninit` also sits cleanly in D/IRAM).
//! The remaining memory has no hardware permissions, so the backend applies none:
//! images are trusted (they run only through the developer `native-dev` handler).

use core::{alloc::Layout, mem::MaybeUninit, ptr::NonNull};

use memory::{CapabilityFlags, ManagerTrait, RegionDescriptor, RegionManager};
use native_loader::{Error, Region};

const DESCRIPTOR: RegionDescriptor = RegionDescriptor::new(CapabilityFlags::Executable);

#[cfg(feature = "esp32s3")]
const DIRAM_DRAM_START: usize = 0x3FC8_8000;
#[cfg(feature = "esp32s3")]
const DIRAM_DRAM_END: usize = 0x3FCF_0000;
#[cfg(feature = "esp32s3")]
const IRAM_OFFSET: usize = 0x006F_0000;

#[cfg(feature = "esp32s2")]
const DIRAM_DRAM_START: usize = 0x3FFB_0000;
#[cfg(feature = "esp32s2")]
const DIRAM_DRAM_END: usize = 0x4000_0000;
#[cfg(feature = "esp32s2")]
const IRAM_OFFSET: usize = 0x0007_0000;

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
#[cfg(target_arch = "riscv32")]
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
#[cfg(target_arch = "riscv32")]
fn overlaps(start: usize, size: usize, ranges: &[(usize, usize)]) -> bool {
    let Some(end) = start.checked_add(size) else {
        return true;
    };
    ranges
        .iter()
        .any(|&(range_start, range_end)| start < range_end && range_start < end)
}

/// Whether `[start, start + size)` is non-empty, does not overflow, lies wholly
/// inside internal D/IRAM, and its IRAM alias lies after firmware `.rwtext`.
#[cfg(any(feature = "esp32s2", feature = "esp32s3"))]
fn is_valid_diram_arena(start: usize, size: usize) -> bool {
    if size == 0 {
        return false;
    }
    let Some(end) = start.checked_add(size) else {
        return false;
    };
    if start < DIRAM_DRAM_START || end > DIRAM_DRAM_END {
        return false;
    }
    unsafe extern "C" {
        static _rwtext_end: u8;
    }
    let iram_start = start + IRAM_OFFSET;
    let rwtext_end = (&raw const _rwtext_end) as usize;
    iram_start >= rwtext_end
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
    /// that esp-hal protects against execution or writing on RISC-V, falls outside
    /// internal D/IRAM (or aliases `.rwtext`) on ESP32-S2/S3 (see the
    /// [module documentation](self)), is too small, or the backend already has one.
    ///
    /// # Safety
    ///
    /// The range must be internal SRAM of the selected chip — unified SRAM on
    /// RISC-V, or internal D/IRAM data-bus addresses on ESP32-S2/S3 — reserved
    /// exclusively for this backend for the lifetime of the firmware. On the P4,
    /// other code must coordinate access to the shared cache synchronization
    /// registers with this backend.
    pub unsafe fn initialize(&self, start: *mut u8, size: usize) -> bool {
        if self.arena.is_region_initialized(0) {
            return false;
        }
        #[cfg(target_arch = "riscv32")]
        if overlaps(start as usize, size, &protected_ranges()) {
            return false;
        }
        #[cfg(any(feature = "esp32s2", feature = "esp32s3"))]
        if !is_valid_diram_arena(start as usize, size) {
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

/// Write back the L1 data cache for an internal SRAM range.
#[cfg(feature = "esp32p4")]
#[esp_hal::ram]
unsafe fn write_back_data_cache_twice(start: u32, size: u32) {
    const CACHE_BASE: usize = 0x3ff1_0000;
    const SYNC_CONTROL: *mut u32 = (CACHE_BASE + 0x98) as *mut u32;
    const SYNC_MAP: *mut u32 = (CACHE_BASE + 0x9c) as *mut u32;
    const SYNC_ADDRESS: *mut u32 = (CACHE_BASE + 0xa0) as *mut u32;
    const SYNC_SIZE: *mut u32 = (CACHE_BASE + 0xa4) as *mut u32;
    const L1_DATA_CACHE: u32 = 1 << 4;
    const WRITEBACK_ENABLE: u32 = 1 << 2;
    const SYNC_DONE: u32 = 1 << 4;

    unsafe extern "C" {
        fn Cache_Get_L1_DCache_Line_Size() -> u32;
    }

    // SAFETY: the caller supplies an internal SRAM range and serializes access to
    // the cache controller. The ROM reports a power-of-two cache line size.
    unsafe {
        let line_mask = Cache_Get_L1_DCache_Line_Size() - 1;
        let aligned_start = start & !line_mask;
        let aligned_size = (size + (start & line_mask) + line_mask) & !line_mask;

        // Order the loader's stores before programming the cache controller.
        core::arch::asm!("fence iorw, iorw", options(nostack));
        SYNC_MAP.write_volatile(L1_DATA_CACHE);
        SYNC_ADDRESS.write_volatile(aligned_start);
        SYNC_SIZE.write_volatile(aligned_size);

        // Espressif's IRAM workaround programs the range once and waits for each
        // writeback before proceeding. A single synchronization can lose writes.
        // https://github.com/espressif/esp-idf/commit/1f559a55588d687b539e3128e50025c2b99eefb1
        for _ in 0..2 {
            SYNC_CONTROL.write_volatile(WRITEBACK_ENABLE);
            while SYNC_CONTROL.read_volatile() & SYNC_DONE == 0 {}
        }
    }
}

/// Make the instructions written to `[start, start + size)` visible to instruction
/// fetch.
fn synchronize_instruction_fetch(start: usize, size: usize) -> Result<(), Error> {
    #[cfg(feature = "esp32p4")]
    {
        const CACHE_MAP_L1_ICACHE_0: u32 = 1 << 0;
        const CACHE_MAP_L1_ICACHE_1: u32 = 1 << 1;

        unsafe extern "C" {
            fn Cache_Invalidate_Addr(bus: u32, address: u32, size: u32) -> core::ffi::c_int;
        }

        // Both operations use the same synchronization registers. Keep them
        // together with interrupts masked and this backend's callers serialized.
        critical_section::with(|_| {
            // SAFETY: this range is mapped internal SRAM and the critical section
            // serializes use by this backend. Cache maintenance preserves its contents.
            unsafe {
                write_back_data_cache_twice(start as u32, size as u32);
                if Cache_Invalidate_Addr(
                    CACHE_MAP_L1_ICACHE_0 | CACHE_MAP_L1_ICACHE_1,
                    start as u32,
                    size as u32,
                ) != 0
                {
                    return Err(Error::FinalizationFailed);
                }
            }
            Ok(())
        })?;
    }
    #[cfg(not(feature = "esp32p4"))]
    let _ = (start, size);

    #[cfg(target_arch = "riscv32")]
    // SAFETY: fences have no memory-safety preconditions.
    unsafe {
        core::arch::asm!("fence", "fence.i", options(nostack));
    }
    #[cfg(target_arch = "xtensa")]
    // SAFETY: memory and instruction barriers have no memory-safety preconditions.
    unsafe {
        core::arch::asm!("memw", "isync", options(nostack));
    }
    Ok(())
}

/// Round an allocation layout up to 4-byte alignment and 4-byte size on
/// split-bus Xtensa chips, where IRAM accesses must be 32-bit word-aligned.
#[cfg(any(feature = "esp32s2", feature = "esp32s3"))]
fn allocation_layout(layout: Layout) -> Result<Layout, Error> {
    let align = layout.align().max(4);
    let size = layout
        .size()
        .checked_add(3)
        .map(|n| n & !3)
        .ok_or(Error::AllocationFailed)?;
    Layout::from_size_align(size, align).map_err(|_| Error::AllocationFailed)
}

/// Ensure executable bytes written through DRAM are also visible at the IRAM
/// execution alias.
///
/// On real ESP32-S2/S3 silicon, DRAM and IRAM alias the same physical SRAM, so
/// each 32-bit IRAM read already matches DRAM and no IRAM store is performed.
/// Espressif's QEMU models `dram` and `iram` as distinct backing memory regions,
/// so mirroring differing 32-bit words into IRAM makes loaded code visible to
/// instruction fetch under emulation as well.
///
/// # Safety
///
/// - `dram_base` and `iram_base` must be 4-byte aligned pointers to the DRAM
///   allocation and its matching IRAM execution alias for a live allocation
///   whose rounded byte size `rounded_size` is a multiple of 4.
/// - `offset <= end` and `end <= rounded_size` (so `end.next_multiple_of(4)`
///   does not overflow and `word_start..word_end` lies wholly within
///   `0..rounded_size` in both DRAM and IRAM).
/// - Every byte in `dram_base[word_start..word_end]` must be initialized
///   (guaranteed by zeroing the full rounded allocation in
///   [`ExecutableMemory::allocate`]).
#[cfg(any(feature = "esp32s2", feature = "esp32s3"))]
unsafe fn mirror_executable_region_to_iram(
    dram_base: NonNull<u8>,
    iram_base: NonNull<u8>,
    offset: usize,
    end: usize,
) {
    // Order prior DRAM writes before reading back through the IRAM alias.
    // SAFETY: `memw` has no memory-safety preconditions.
    unsafe { core::arch::asm!("memw", options(nostack)) };

    let word_start = offset & !3;
    let word_end = end.next_multiple_of(4);
    let mut current = word_start;
    while current < word_end {
        // SAFETY: by the function's safety contract, `dram_base` and `iram_base`
        // are 4-byte aligned, `current..current + 4` is 4-byte aligned and in
        // bounds of the rounded allocation in both DRAM and IRAM, and all bytes
        // in `dram_base[word_start..word_end]` are initialized.
        unsafe {
            let word = dram_base.as_ptr().add(current).cast::<u32>().read();
            let iram_word = iram_base.as_ptr().add(current).cast::<u32>();
            if iram_word.read_volatile() != word {
                iram_word.write_volatile(word);
            }
        }
        current += 4;
    }
}

// SAFETY: the arena hands out exclusive blocks of the requested layout. On
// unified-bus RISC-V chips the CPU reads, writes and executes at the same
// addresses; on split-bus ESP32-S2/S3 `execution_base` returns the IRAM alias
// of the same physical D/IRAM bytes. Finalization keeps the block and its
// contents, and makes the writes visible to instruction fetch.
unsafe impl native_loader::ExecutableMemory for ExecutableMemory {
    fn allocate(&self, layout: Layout) -> Result<NonNull<u8>, Error> {
        if layout.size() == 0 {
            return Err(Error::AllocationFailed);
        }
        #[cfg(any(feature = "esp32s2", feature = "esp32s3"))]
        {
            let rounded = allocation_layout(layout)?;
            // SAFETY: the block is released through `free` with the same rounded layout.
            let ptr = unsafe { self.arena.allocate(CapabilityFlags::Executable, rounded) }
                .ok_or(Error::AllocationFailed)?;
            // SAFETY: `ptr` points to an exclusive writable allocation of `rounded.size()`
            // bytes. Zeroing the full rounded allocation ensures
            // `mirror_executable_region_to_iram` never reads uninitialized padding or tail
            // bytes as `u32`, even for non-4-byte-aligned image sizes or unaligned offsets.
            unsafe { ptr.as_ptr().write_bytes(0, rounded.size()) };
            Ok(ptr)
        }
        #[cfg(not(any(feature = "esp32s2", feature = "esp32s3")))]
        {
            // SAFETY: the block is released through `free` with the same layout.
            unsafe { self.arena.allocate(CapabilityFlags::Executable, layout) }
                .ok_or(Error::AllocationFailed)
        }
    }

    #[cfg(any(feature = "esp32s2", feature = "esp32s3"))]
    fn execution_base(&self, allocation: NonNull<u8>) -> NonNull<u8> {
        // SAFETY: `initialize` guarantees the arena lies wholly within
        // `[DIRAM_DRAM_START, DIRAM_DRAM_END]`, so adding `IRAM_OFFSET` yields
        // the non-null instruction-bus alias of the same physical bytes.
        let address = allocation.as_ptr().addr() + IRAM_OFFSET;
        NonNull::new(allocation.as_ptr().with_addr(address))
            .expect("D/IRAM instruction alias is non-null")
    }

    unsafe fn finish_writes(
        &self,
        allocation: NonNull<u8>,
        layout: Layout,
        regions: &mut dyn Iterator<Item = Region>,
    ) -> Result<(), Error> {
        #[cfg(any(feature = "esp32s2", feature = "esp32s3"))]
        let iram_base = self.execution_base(allocation);

        for region in regions {
            let end = region
                .offset
                .checked_add(region.size)
                .ok_or(Error::FinalizationFailed)?;
            if end > layout.size() {
                return Err(Error::FinalizationFailed);
            }
            #[cfg(any(feature = "esp32s2", feature = "esp32s3"))]
            if region.executable && region.size > 0 {
                // SAFETY: `allocate` rounded `layout` to 4-byte alignment and
                // size and zero-initialized the entire rounded block,
                // `iram_base` is its matching IRAM alias, and
                // `region.offset <= end <= layout.size() <= rounded.size()`.
                unsafe {
                    mirror_executable_region_to_iram(allocation, iram_base, region.offset, end);
                }
            }
        }
        synchronize_instruction_fetch(allocation.as_ptr() as usize, layout.size())
    }

    unsafe fn free(&self, allocation: NonNull<u8>, layout: Layout) {
        #[cfg(any(feature = "esp32s2", feature = "esp32s3"))]
        let layout = allocation_layout(layout).expect("allocated layout was already rounded");
        // SAFETY: the caller passes a block of this backend, which nothing uses any more.
        unsafe { self.arena.deallocate(allocation, layout) };
    }
}
