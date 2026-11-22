//! C allocation hooks used by esp-radio and esp-rtos when esp-alloc is disabled.

use core::{
    alloc::Layout,
    ptr::{self, NonNull},
};

use memory::{CapabilityFlags, ManagerTrait};

use crate::memory::MemoryManager;

use super::INTERNAL_HEAP;

const ALIGNMENT: usize = 8;

fn heap() -> &'static MemoryManager {
    INTERNAL_HEAP
        .try_get()
        .expect("Wi-Fi heap must be installed before the scheduler")
}

pub(super) fn free_bytes(heap: &MemoryManager) -> usize {
    heap.get_statistics().region_stats[0]
        .as_ref()
        .map_or(0, |region| region.free)
}

fn layout(size: usize) -> Option<Layout> {
    Layout::from_size_align(ALIGNMENT.checked_add(size)?, ALIGNMENT).ok()
}

#[unsafe(export_name = "malloc_internal")]
extern "C" fn allocate(size: usize) -> *mut u8 {
    let Some(layout) = layout(size) else {
        return ptr::null_mut();
    };
    let capabilities = CapabilityFlags::DirectMemoryAccess | CapabilityFlags::AlwaysAccessible;
    // SAFETY: the allocation holds an aligned size header and the C caller's payload.
    unsafe {
        let Some(pointer) = heap().allocate(capabilities, layout) else {
            return ptr::null_mut();
        };
        pointer.as_ptr().cast::<usize>().write(size);
        pointer.as_ptr().add(ALIGNMENT)
    }
}

#[unsafe(export_name = "malloc")]
extern "C" fn allocate_default(size: usize) -> *mut u8 {
    allocate(size)
}

#[unsafe(export_name = "free_internal")]
unsafe extern "C" fn release(pointer: *mut u8) {
    if let Some(pointer) = NonNull::new(pointer) {
        // SAFETY: the C caller supplied a live pointer from our allocation hooks.
        unsafe {
            let header = pointer.sub(ALIGNMENT);
            let size = header.cast::<usize>().read();
            heap().deallocate(header, layout(size).unwrap());
        }
    }
}

#[unsafe(export_name = "free")]
unsafe extern "C" fn release_default(pointer: *mut u8) {
    // SAFETY: the caller follows the same contract as free_internal.
    unsafe { release(pointer) };
}

#[unsafe(export_name = "realloc_internal")]
unsafe extern "C" fn reallocate(pointer: *mut u8, size: usize) -> *mut u8 {
    let Some(pointer) = NonNull::new(pointer) else {
        return allocate(size);
    };
    if size == 0 {
        // SAFETY: the C caller supplied a live pointer from our allocation hooks.
        unsafe { release(pointer.as_ptr()) };
        return ptr::null_mut();
    }
    let Some(new_layout) = layout(size) else {
        return ptr::null_mut();
    };
    let capabilities = CapabilityFlags::DirectMemoryAccess | CapabilityFlags::AlwaysAccessible;
    // SAFETY: the C caller supplied a live pointer from our allocation hooks. The allocator
    // preserves the original block on failure and takes ownership of it on success.
    unsafe {
        let header = pointer.sub(ALIGNMENT);
        let old_size = header.cast::<usize>().read();
        let Some(header) =
            heap().reallocate(capabilities, header, layout(old_size).unwrap(), new_layout)
        else {
            return ptr::null_mut();
        };
        header.as_ptr().cast::<usize>().write(size);
        header.as_ptr().add(ALIGNMENT)
    }
}

#[unsafe(export_name = "calloc_internal")]
extern "C" fn allocate_zeroed(number: u32, size: usize) -> *mut u8 {
    let Some(size) = (number as usize).checked_mul(size) else {
        return ptr::null_mut();
    };
    let pointer = allocate(size);
    if !pointer.is_null() {
        // SAFETY: the successful allocation holds at least size writable bytes.
        unsafe { pointer.write_bytes(0, size) };
    }
    pointer
}

#[unsafe(export_name = "calloc")]
extern "C" fn allocate_zeroed_default(number: u32, size: usize) -> *mut u8 {
    allocate_zeroed(number, size)
}

#[unsafe(export_name = "get_free_internal_heap_size")]
extern "C" fn free_internal_bytes() -> usize {
    free_bytes(heap())
}
