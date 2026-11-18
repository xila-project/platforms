use core::{cell::RefCell, mem::MaybeUninit, ptr::NonNull};
use linked_list_allocator::Heap;
use memory::{CapabilityFlags, Layout, ManagerTrait};
use synchronization::blocking_mutex::CriticalSectionMutex;

/// Fixed-region memory manager for Cortex-M bare-metal using `linked_list_allocator`.
pub struct MemoryManager {
    heap: CriticalSectionMutex<RefCell<Heap>>,
}

unsafe impl Send for MemoryManager {}
unsafe impl Sync for MemoryManager {}

impl Default for MemoryManager {
    fn default() -> Self {
        Self::new()
    }
}

impl MemoryManager {
    /// Create a new uninitialized `MemoryManager`.
    pub const fn new() -> Self {
        Self {
            heap: CriticalSectionMutex::new(RefCell::new(Heap::empty())),
        }
    }

    /// Initialize the managed heap region with a raw memory pointer and length.
    ///
    /// # Safety
    /// The caller must ensure that `start` points to valid, unaliased memory of at least
    /// `size` bytes and is properly aligned.
    pub unsafe fn init(&self, start: *mut u8, size: usize) {
        self.heap.lock(|heap| {
            unsafe { heap.borrow_mut().init(start, size) };
        });
    }

    /// Initialize the managed heap region from a static mutable slice.
    pub fn init_from_slice(&self, slice: &'static mut [MaybeUninit<u8>]) {
        let size = slice.len();
        let ptr = slice.as_mut_ptr() as *mut u8;
        unsafe { self.init(ptr, size) };
    }
}

impl ManagerTrait for MemoryManager {
    unsafe fn allocate(
        &self,
        capabilities: CapabilityFlags,
        layout: Layout,
    ) -> Option<NonNull<u8>> {
        // Executable memory allocations cannot be satisfied on this target.
        if capabilities.contains(CapabilityFlags::Executable) {
            return None;
        }

        self.heap
            .lock(|heap| heap.borrow_mut().allocate_first_fit(layout).ok())
    }

    unsafe fn deallocate(&self, pointer: NonNull<u8>, layout: Layout) {
        self.heap.lock(|heap| {
            unsafe { heap.borrow_mut().deallocate(pointer, layout) };
        });
    }

    fn get_used(&self) -> usize {
        self.heap.lock(|heap| heap.borrow().used())
    }

    fn get_free(&self) -> usize {
        self.heap.lock(|heap| heap.borrow().free())
    }

    fn get_page_size(&self) -> usize {
        // Cortex-M4 has no hardware MMU page translation for RAM;
        // minimum word alignment for memory operations is 4 bytes.
        4
    }
}

#[macro_export]
macro_rules! instantiate_global_allocator {
    ($manager:expr) => {
        $crate::memory_exported::instantiate_global_allocator!($manager);
    };
    () => {
        pub static __DRIVERS_NORDIC_MEMORY_ALLOCATOR: $crate::memory::MemoryManager =
            $crate::memory::MemoryManager::new();

        $crate::memory_exported::instantiate_global_allocator!(&__DRIVERS_NORDIC_MEMORY_ALLOCATOR);
    };
}

pub use instantiate_global_allocator;
