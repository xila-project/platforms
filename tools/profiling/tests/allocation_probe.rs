//! Fixture of `heap-smoke` (`src/heap_smoke.rs`), which builds it as the main
//! function of a scratch crate against `drivers/std` and Core's locked memory
//! subsystem. It is not a Cargo test of this package.
use drivers_std::memory::MemoryManager;
use memory::{CapabilityFlags, Layout, ManagerTrait};

fn main() {
    let manager = MemoryManager::new();
    unsafe {
        let layout = Layout::from_size_align(37, 32).unwrap();
        assert!(
            manager
                .allocate(CapabilityFlags::DirectMemoryAccess, layout)
                .is_none()
        );
        let first = manager.allocate(CapabilityFlags::empty(), layout).unwrap();
        assert_eq!(first.as_ptr() as usize % 32, 0);
        first.as_ptr().write(42);
        let next_layout = Layout::from_size_align(79, 32).unwrap();
        let second = manager
            .reallocate(CapabilityFlags::empty(), first, layout, next_layout)
            .unwrap();
        assert_eq!(second.as_ptr().read(), 42);
        manager.deallocate(second, next_layout);
        assert_eq!(manager.get_used(), 0);
    }
}
