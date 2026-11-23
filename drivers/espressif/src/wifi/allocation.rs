//! C allocation hooks used by esp-radio and esp-rtos when esp-alloc is disabled.

use core::{
    future::{Future, poll_fn},
    pin::pin,
    ptr::{self, NonNull},
    sync::atomic::{AtomicBool, AtomicPtr, Ordering},
};

use memory::{CapabilityFlags, ManagerTrait};

use crate::memory::MemoryManager;

use super::{
    INTERNAL_HEAP,
    layout::{ALIGNMENT, layout},
};

// All scheduled vendor threads run on the first core. Allocations in the main
// thread are scoped to calls and single polls of the vendor library, so normal
// Xila tasks can keep allocating in PSRAM while a radio future is suspended.
static MAIN_TASK: AtomicPtr<()> = AtomicPtr::new(ptr::null_mut());
static INTERNAL_SCOPE: AtomicBool = AtomicBool::new(false);

pub(crate) fn internal_allocation_required() -> bool {
    if INTERNAL_SCOPE.load(Ordering::Relaxed) {
        return true;
    }
    let main = MAIN_TASK.load(Ordering::Acquire);
    !main.is_null()
        && (!esp_hal::interrupt::RunLevel::current().is_thread()
            || esp_radio_rtos_driver::current_task().as_ptr() != main)
}

pub(super) fn record_main_task() {
    MAIN_TASK.store(
        esp_radio_rtos_driver::current_task().as_ptr(),
        Ordering::Release,
    );
}

pub(super) fn internal<T>(operation: impl FnOnce() -> T) -> T {
    struct Restore(bool);
    impl Drop for Restore {
        fn drop(&mut self) {
            INTERNAL_SCOPE.store(self.0, Ordering::Relaxed);
        }
    }

    let _restore = Restore(INTERNAL_SCOPE.swap(true, Ordering::Relaxed));
    operation()
}

pub(super) async fn poll_internal<F: Future>(future: F) -> F::Output {
    let mut future = pin!(future);
    poll_fn(|context| internal(|| future.as_mut().poll(context))).await
}

fn heap() -> Option<&'static MemoryManager> {
    INTERNAL_HEAP.try_get().copied()
}

pub(super) fn free_bytes(heap: &MemoryManager) -> usize {
    heap.get_statistics().region_stats[0]
        .as_ref()
        .map_or(0, |region| region.free)
}

#[unsafe(export_name = "malloc_internal")]
extern "C" fn allocate(size: usize) -> *mut u8 {
    let (Some(heap), Some(layout)) = (heap(), layout(size)) else {
        return ptr::null_mut();
    };
    let capabilities = CapabilityFlags::DirectMemoryAccess | CapabilityFlags::AlwaysAccessible;
    // SAFETY: the allocation holds an aligned size header and the C caller's payload.
    unsafe {
        let Some(pointer) = heap.allocate(capabilities, layout) else {
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
    if let (Some(heap), Some(pointer)) = (heap(), NonNull::new(pointer)) {
        // SAFETY: the C caller supplied a live pointer from our allocation hooks.
        unsafe {
            let header = pointer.sub(ALIGNMENT);
            let size = header.cast::<usize>().read();
            if let Some(layout) = layout(size) {
                heap.deallocate(header, layout);
            }
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
    let (Some(heap), Some(new_layout)) = (heap(), layout(size)) else {
        return ptr::null_mut();
    };
    let capabilities = CapabilityFlags::DirectMemoryAccess | CapabilityFlags::AlwaysAccessible;
    // SAFETY: the C caller supplied a live pointer from our allocation hooks. The allocator
    // preserves the original block on failure and takes ownership of it on success.
    unsafe {
        let header = pointer.sub(ALIGNMENT);
        let old_size = header.cast::<usize>().read();
        let Some(old_layout) = layout(old_size) else {
            return ptr::null_mut();
        };
        let Some(header) = heap.reallocate(capabilities, header, old_layout, new_layout) else {
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
    heap().map_or(0, free_bytes)
}
