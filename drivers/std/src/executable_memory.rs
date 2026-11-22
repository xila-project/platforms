//! Executable memory for the native loader on hosted Unix systems.
//!
//! Each image gets its own anonymous private mapping, mapped read and write while
//! the loader copies and relocates it. [`finish_writes`] then applies the final
//! permissions page by page and never leaves a page both writable and executable
//! (W^X): code pages become read and execute, data pages read and write, read-only
//! pages read only, and the gaps between segments become inaccessible. A page that
//! would need both write and execute permissions makes finalization fail, so images
//! must place their writable segments on their own pages (the SDK uses a page as its
//! data alignment on hosts).
//!
//! After the permission change, the instruction cache is synchronized with the
//! written code where the architecture requires it: x86-64 keeps instruction fetch
//! coherent with data writes, AArch64 does not.
//!
//! [`finish_writes`]: native_loader::ExecutableMemory::finish_writes

use core::{alloc::Layout, ptr::NonNull};

use libc::{
    _SC_PAGE_SIZE, MAP_ANONYMOUS, MAP_FAILED, MAP_PRIVATE, PROT_EXEC, PROT_NONE, PROT_READ,
    PROT_WRITE, mmap, mprotect, munmap, sysconf,
};
use native_loader::{Error, Region};

/// The program-header limit of the native loader, which bounds the region count.
const MAXIMUM_REGIONS: usize = 16;

/// Executable memory backed by one anonymous mapping per loaded image.
///
/// The type holds no state: every allocation is its own mapping, released by
/// [`free`](native_loader::ExecutableMemory::free).
#[derive(Debug, Default, Clone, Copy)]
pub struct ExecutableMemory;

impl ExecutableMemory {
    pub const fn new() -> Self {
        Self
    }
}

fn page_size() -> usize {
    // SAFETY: `sysconf` has no preconditions.
    let size = unsafe { sysconf(_SC_PAGE_SIZE) };
    usize::try_from(size)
        .ok()
        .filter(|size| size.is_power_of_two())
        .unwrap_or(4096)
}

/// Linux can implicitly add execute permission to any readable mapping.
#[cfg(target_os = "linux")]
fn explicit_execute_permissions() -> bool {
    // SAFETY: this argument only queries the calling thread's personality.
    let personality = unsafe { libc::personality(0xffff_ffff) };
    personality >= 0 && personality & libc::READ_IMPLIES_EXEC == 0
}

#[cfg(not(target_os = "linux"))]
fn explicit_execute_permissions() -> bool {
    true
}

/// The mapped length of an allocation: its size rounded up to whole pages.
fn mapped_size(layout: Layout, page_size: usize) -> Option<usize> {
    layout
        .size()
        .checked_next_multiple_of(page_size)
        .filter(|size| *size > 0)
}

/// The protection a page needs, given the regions that overlap it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct PagePermissions {
    readable: bool,
    writable: bool,
    executable: bool,
}

impl PagePermissions {
    fn include(&mut self, region: &Region) {
        self.readable = true;
        self.writable |= region.writable;
        self.executable |= region.executable;
    }

    fn protection(self) -> Result<i32, Error> {
        match (self.readable, self.writable, self.executable) {
            (_, true, true) => Err(Error::FinalizationFailed),
            (false, _, _) => Ok(PROT_NONE),
            (true, false, false) => Ok(PROT_READ),
            (true, true, false) => Ok(PROT_READ | PROT_WRITE),
            (true, false, true) => Ok(PROT_READ | PROT_EXEC),
        }
    }
}

/// Compute the protection of every page in `[0, size)`, as runs of consecutive pages
/// with the same protection, in ascending order.
///
/// `visit` receives the offset, length and protection of each run.
fn protection_runs(
    regions: &[Region],
    size: usize,
    page_size: usize,
    mut visit: impl FnMut(usize, usize, i32) -> Result<(), Error>,
) -> Result<(), Error> {
    for region in regions {
        let end = region
            .offset
            .checked_add(region.size)
            .ok_or(Error::FinalizationFailed)?;
        if end > size {
            return Err(Error::FinalizationFailed);
        }
    }
    let mut run: Option<(usize, i32)> = None;
    let mut offset = 0;
    while offset < size {
        let page_end = offset + page_size;
        let mut permissions = PagePermissions::default();
        for region in regions.iter().filter(|region| {
            region.size > 0 && region.offset < page_end && offset < region.offset + region.size
        }) {
            permissions.include(region);
        }
        let protection = permissions.protection()?;
        match run {
            Some((_, current)) if current == protection => {}
            Some((start, current)) => {
                visit(start, offset - start, current)?;
                run = Some((offset, protection));
            }
            None => run = Some((offset, protection)),
        }
        offset = page_end;
    }
    if let Some((start, protection)) = run {
        visit(start, size - start, protection)?;
    }
    Ok(())
}

/// Make instruction fetch observe the instructions written to `[start, start + size)`.
#[cfg(target_arch = "aarch64")]
fn synchronize_instruction_cache(start: *const u8, size: usize) {
    #[cfg(target_vendor = "apple")]
    {
        unsafe extern "C" {
            fn sys_icache_invalidate(start: *mut core::ffi::c_void, length: usize);
        }
        // SAFETY: the range is mapped and readable.
        unsafe { sys_icache_invalidate(start.cast_mut().cast(), size) };
    }
    #[cfg(not(target_vendor = "apple"))]
    {
        use core::arch::asm;

        let cache_type: u64;
        // SAFETY: Linux lets user space read CTR_EL0.
        unsafe { asm!("mrs {}, ctr_el0", out(reg) cache_type, options(nomem, nostack)) };
        // CTR_EL0 holds log2 of the smallest line size, in words.
        let data_line = 4usize << ((cache_type >> 16) & 0xf);
        let instruction_line = 4usize << (cache_type & 0xf);
        let address = start as usize;
        let end = address + size;

        // Clean the data cache to the point of unification, unless CTR_EL0.IDC says
        // it is not required.
        if cache_type & (1 << 28) == 0 {
            let mut line = address & !(data_line - 1);
            while line < end {
                // SAFETY: cleaning a mapped line has no effect on its contents.
                unsafe { asm!("dc cvau, {}", in(reg) line, options(nostack)) };
                line += data_line;
            }
        }
        // SAFETY: barriers have no memory-safety preconditions.
        unsafe { asm!("dsb ish", options(nostack)) };
        // Invalidate the instruction cache, unless CTR_EL0.DIC says it is not required.
        if cache_type & (1 << 29) == 0 {
            let mut line = address & !(instruction_line - 1);
            while line < end {
                // SAFETY: invalidating instruction cache lines is always safe.
                unsafe { asm!("ic ivau, {}", in(reg) line, options(nostack)) };
                line += instruction_line;
            }
            // SAFETY: as above.
            unsafe { asm!("dsb ish", options(nostack)) };
        }
        // SAFETY: as above.
        unsafe { asm!("isb", options(nostack)) };
    }
}

/// x86 keeps instruction fetch coherent with data writes, and the permission change
/// is a system call, which serializes this thread.
#[cfg(not(target_arch = "aarch64"))]
fn synchronize_instruction_cache(_start: *const u8, _size: usize) {}

// SAFETY: allocations are exclusive private mappings, readable and writable at the
// same address used for execution, aligned to the layout. Finalization keeps the
// mapping and its contents, applies W^X permissions and synchronizes the
// instruction cache. Only `free` unmaps.
unsafe impl native_loader::ExecutableMemory for ExecutableMemory {
    fn allocate(&self, layout: Layout) -> Result<NonNull<u8>, Error> {
        if !explicit_execute_permissions() {
            return Err(Error::AllocationFailed);
        }
        let page_size = page_size();
        let size = mapped_size(layout, page_size).ok_or(Error::AllocationFailed)?;
        // Mappings are page aligned. For a larger alignment, map enough to contain an
        // aligned range and unmap the excess on both sides.
        let alignment = layout.align().max(page_size);
        let excess = alignment - page_size;
        let reserved = size.checked_add(excess).ok_or(Error::AllocationFailed)?;
        // SAFETY: an anonymous private mapping aliases no existing memory.
        let pointer = unsafe {
            mmap(
                core::ptr::null_mut(),
                reserved,
                PROT_READ | PROT_WRITE,
                MAP_PRIVATE | MAP_ANONYMOUS,
                -1,
                0,
            )
        };
        if pointer == MAP_FAILED {
            return Err(Error::AllocationFailed);
        }
        let Some(allocation) = NonNull::new(pointer.cast::<u8>()) else {
            // SAFETY: mmap succeeded, but a null address cannot be returned.
            unsafe { munmap(pointer, reserved) };
            return Err(Error::AllocationFailed);
        };
        let leading = allocation.as_ptr().align_offset(alignment);
        let trailing = excess - leading;
        // SAFETY: the aligned start lies within the mapping.
        let aligned = unsafe { allocation.add(leading) };
        // SAFETY: both ranges are whole pages of the mapping above, outside the
        // returned allocation. On failure, release only pages still owned here.
        unsafe {
            if leading > 0 && munmap(pointer, leading) != 0 {
                munmap(pointer, reserved);
                return Err(Error::AllocationFailed);
            }
            if trailing > 0 && munmap(aligned.as_ptr().add(size).cast(), trailing) != 0 {
                munmap(aligned.as_ptr().cast(), size + trailing);
                return Err(Error::AllocationFailed);
            }
        }
        Ok(aligned)
    }

    unsafe fn finish_writes(
        &self,
        allocation: NonNull<u8>,
        layout: Layout,
        regions: &mut dyn Iterator<Item = Region>,
    ) -> Result<(), Error> {
        if !explicit_execute_permissions() {
            return Err(Error::FinalizationFailed);
        }
        let page_size = page_size();
        let size = mapped_size(layout, page_size).ok_or(Error::FinalizationFailed)?;
        let mut collected = [Region {
            offset: 0,
            size: 0,
            executable: false,
            writable: false,
        }; MAXIMUM_REGIONS];
        let mut count = 0;
        for region in regions {
            *collected.get_mut(count).ok_or(Error::FinalizationFailed)? = region;
            count += 1;
        }
        let regions = &collected[..count];

        // Check every page before changing any, so that a W^X violation leaves the
        // allocation as it was.
        protection_runs(regions, size, page_size, |_, _, _| Ok(()))?;
        protection_runs(regions, size, page_size, |offset, length, protection| {
            // SAFETY: the run lies within the allocation, which this backend mapped.
            let address = unsafe { allocation.as_ptr().add(offset) };
            if unsafe { mprotect(address.cast(), length, protection) } != 0 {
                return Err(Error::FinalizationFailed);
            }
            if protection & PROT_EXEC != 0 {
                synchronize_instruction_cache(address, length);
            }
            Ok(())
        })
    }

    unsafe fn free(&self, allocation: NonNull<u8>, layout: Layout) {
        if let Some(size) = mapped_size(layout, page_size()) {
            // SAFETY: the caller passes an allocation of this backend, which nothing
            // references any more.
            unsafe { munmap(allocation.as_ptr().cast(), size) };
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use native_loader::ExecutableMemory as _;

    fn region(offset: usize, size: usize, executable: bool, writable: bool) -> Region {
        Region {
            offset,
            size,
            executable,
            writable,
        }
    }

    fn runs(regions: &[Region], size: usize) -> Result<Vec<(usize, usize, i32)>, Error> {
        let mut runs = Vec::new();
        protection_runs(regions, size, 4096, |offset, length, protection| {
            runs.push((offset, length, protection));
            Ok(())
        })?;
        Ok(runs)
    }

    #[test]
    fn pages_take_the_union_of_their_regions() {
        let regions = [
            region(0, 4096, false, false),
            region(4096, 100, true, false),
            region(4196, 3900, false, false),
            region(3 * 4096, 10, false, true),
        ];
        assert_eq!(
            runs(&regions, 4 * 4096),
            Ok(vec![
                (0, 4096, PROT_READ),
                (4096, 4096, PROT_READ | PROT_EXEC),
                (2 * 4096, 4096, PROT_NONE),
                (3 * 4096, 4096, PROT_READ | PROT_WRITE),
            ])
        );
    }

    #[test]
    fn consecutive_pages_with_the_same_protection_merge() {
        let regions = [region(0, 3 * 4096, true, false)];
        assert_eq!(
            runs(&regions, 3 * 4096),
            Ok(vec![(0, 3 * 4096, PROT_READ | PROT_EXEC)])
        );
    }

    #[test]
    fn writable_code_on_one_page_is_rejected() {
        let regions = [region(0, 100, true, false), region(100, 100, false, true)];
        assert_eq!(runs(&regions, 4096), Err(Error::FinalizationFailed));
        let regions = [region(0, 4096, true, true)];
        assert_eq!(runs(&regions, 4096), Err(Error::FinalizationFailed));
    }

    #[test]
    fn regions_outside_the_allocation_are_rejected() {
        let regions = [region(4000, 200, false, false)];
        assert_eq!(runs(&regions, 4096), Err(Error::FinalizationFailed));
        let regions = [region(usize::MAX, 2, false, false)];
        assert_eq!(runs(&regions, 4096), Err(Error::FinalizationFailed));
    }

    #[test]
    fn allocations_respect_large_alignments() {
        let memory = ExecutableMemory::new();
        for alignment in [1, 4096, 16 * 1024, 64 * 1024] {
            let layout = Layout::from_size_align(5000, alignment).unwrap();
            let allocation = memory.allocate(layout).unwrap();
            assert!(allocation.as_ptr().addr().is_multiple_of(alignment));
            // The whole layout is writable.
            unsafe { allocation.as_ptr().write_bytes(0xa5, layout.size()) };
            unsafe { memory.free(allocation, layout) };
        }
    }

    #[test]
    fn empty_layouts_are_refused() {
        let layout = Layout::from_size_align(0, 1).unwrap();
        assert_eq!(
            ExecutableMemory::new().allocate(layout),
            Err(Error::AllocationFailed)
        );
    }

    /// A function that returns 42, in the host's machine code.
    #[cfg(target_arch = "x86_64")]
    const RETURN_42: &[u8] = &[0xb8, 42, 0, 0, 0, 0xc3]; // mov eax, 42; ret
    #[cfg(target_arch = "aarch64")]
    const RETURN_42: &[u8] = &[
        0x40, 0x05, 0x80, 0x52, // mov w0, #42
        0xc0, 0x03, 0x5f, 0xd6, // ret
    ];

    #[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]
    #[test]
    fn written_code_runs_and_data_stays_writable() {
        let page_size = page_size();
        let memory = ExecutableMemory::new();
        let layout = Layout::from_size_align(2 * page_size, page_size).unwrap();
        let allocation = memory.allocate(layout).unwrap();
        unsafe {
            core::ptr::copy_nonoverlapping(RETURN_42.as_ptr(), allocation.as_ptr(), RETURN_42.len())
        };
        let mut regions = [
            region(0, RETURN_42.len(), true, false),
            region(page_size, 8, false, true),
        ]
        .into_iter();
        unsafe { memory.finish_writes(allocation, layout, &mut regions) }.unwrap();

        let function: extern "C" fn() -> i32 = unsafe { core::mem::transmute(allocation.as_ptr()) };
        assert_eq!(function(), 42);
        let data = unsafe { allocation.as_ptr().add(page_size) };
        unsafe { data.write_volatile(7) };
        assert_eq!(unsafe { data.read_volatile() }, 7);
        unsafe { memory.free(allocation, layout) };
    }

    #[test]
    fn a_rejected_finalization_leaves_the_allocation_writable() {
        let memory = ExecutableMemory::new();
        let layout = Layout::from_size_align(100, 16).unwrap();
        let allocation = memory.allocate(layout).unwrap();
        let mut regions = [region(0, 100, true, true)].into_iter();
        assert_eq!(
            unsafe { memory.finish_writes(allocation, layout, &mut regions) },
            Err(Error::FinalizationFailed)
        );
        unsafe { allocation.as_ptr().write_volatile(1) };
        unsafe { memory.free(allocation, layout) };
    }
}
