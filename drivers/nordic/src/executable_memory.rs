//! Executable memory for the native loader on the nRF52840.
//!
//! The nRF52840 SRAM is one memory for instructions and data: the Cortex-M4 fetches
//! instructions from the same `0x2000_0000` addresses the loader writes through,
//! and the ARMv7-M default memory map makes that range executable. Images are
//! therefore allocated from the ordinary heap of [`MemoryManager`].
//!
//! The Cortex-M4 of the nRF52840 has no data cache, and the NVMC instruction cache
//! only caches flash, so finalization only needs barriers: a data synchronization
//! barrier completes the loader's writes and an instruction synchronization barrier
//! discards instructions already prefetched.
//!
//! No permissions are applied: these drivers do not enable the MPU. Enforcing W^X
//! would need one MPU region per segment, with power-of-two sizes aligned to their
//! size, out of eight regions shared with the rest of the firmware. Native images
//! run only through the developer `native-dev` handler, which trusts them.

use core::{alloc::Layout, ptr::NonNull};

use memory::{CapabilityFlags, ManagerTrait};
use native_loader::{Error, Region};

use crate::memory::MemoryManager;

/// Executable memory taken from the firmware heap.
pub struct ExecutableMemory {
    manager: &'static MemoryManager,
}

impl ExecutableMemory {
    /// Allocate images from `manager`, which must manage SRAM.
    pub const fn new(manager: &'static MemoryManager) -> Self {
        Self { manager }
    }
}

#[cfg(all(target_arch = "arm", target_os = "none"))]
fn synchronize_instruction_fetch() {
    cortex_m::asm::dsb();
    cortex_m::asm::isb();
}

#[cfg(not(all(target_arch = "arm", target_os = "none")))]
fn synchronize_instruction_fetch() {
    core::sync::atomic::fence(core::sync::atomic::Ordering::SeqCst);
}

// SAFETY: the heap hands out exclusive SRAM blocks of the requested layout, which the
// core reads, writes and executes at the same addresses. Finalization keeps the block
// and its contents, and makes the writes visible to instruction fetch.
unsafe impl native_loader::ExecutableMemory for ExecutableMemory {
    fn allocate(&self, layout: Layout) -> Result<NonNull<u8>, Error> {
        if layout.size() == 0 {
            return Err(Error::AllocationFailed);
        }
        // SAFETY: the block is released through `free` with the same layout.
        unsafe { self.manager.allocate(CapabilityFlags::Executable, layout) }
            .ok_or(Error::AllocationFailed)
    }

    unsafe fn finish_writes(
        &self,
        _allocation: NonNull<u8>,
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
        synchronize_instruction_fetch();
        Ok(())
    }

    unsafe fn free(&self, allocation: NonNull<u8>, layout: Layout) {
        // SAFETY: the caller passes a block of this backend, which nothing uses any more.
        unsafe { self.manager.deallocate(allocation, layout) };
    }
}

#[cfg(all(test, not(target_os = "none")))]
mod tests {
    use super::*;
    use native_loader::ExecutableMemory as _;
    use std::boxed::Box;

    fn memory() -> ExecutableMemory {
        let manager = Box::leak(Box::new(MemoryManager::new()));
        manager.init_from_slice(Box::leak(Box::<[u8]>::new_uninit_slice(64 * 1024)));
        ExecutableMemory::new(manager)
    }

    fn region(offset: usize, size: usize) -> Region {
        Region {
            offset,
            size,
            executable: true,
            writable: false,
        }
    }

    #[test]
    fn images_come_from_the_heap_and_return_to_it() {
        let memory = memory();
        let layout = Layout::from_size_align(1024, 64).unwrap();
        let allocation = memory.allocate(layout).unwrap();
        assert!(allocation.as_ptr().addr().is_multiple_of(64));
        assert_eq!(memory.manager.get_used(), 1024);
        let mut regions = [region(0, 512), region(512, 512)].into_iter();
        assert_eq!(
            unsafe { memory.finish_writes(allocation, layout, &mut regions) },
            Ok(())
        );
        unsafe { memory.free(allocation, layout) };
        assert_eq!(memory.manager.get_used(), 0);
    }

    #[test]
    fn regions_outside_the_image_are_rejected() {
        let memory = memory();
        let layout = Layout::from_size_align(256, 4).unwrap();
        let allocation = memory.allocate(layout).unwrap();
        let mut regions = [region(200, 100)].into_iter();
        assert_eq!(
            unsafe { memory.finish_writes(allocation, layout, &mut regions) },
            Err(Error::FinalizationFailed)
        );
        unsafe { memory.free(allocation, layout) };
    }

    #[test]
    fn exhaustion_and_empty_layouts_fail() {
        let memory = memory();
        let layout = Layout::from_size_align(1024 * 1024, 4).unwrap();
        assert_eq!(memory.allocate(layout), Err(Error::AllocationFailed));
        let layout = Layout::from_size_align(0, 1).unwrap();
        assert_eq!(memory.allocate(layout), Err(Error::AllocationFailed));
    }
}
