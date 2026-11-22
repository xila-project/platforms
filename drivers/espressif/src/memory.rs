//! Memory manager for Espressif ESP32-family targets with optional external PSRAM.
//!
//! Maps Xila [`CapabilityFlags`] onto internal SRAM and (when supported and attached) external
//! PSRAM using [`RegionManager`]. Board startup provides an exclusive SRAM heap and can
//! register PSRAM after initializing the HAL peripheral. The firmware must link a
//! critical-section implementation (for example, esp-hal's runtime implementation).
//! Allocate cache-off data during normal execution and keep the ISR code and any
//! transitively accessed state accessible during flash operations.

use core::{
    mem::MaybeUninit,
    ptr::NonNull,
    sync::atomic::{AtomicBool, Ordering},
};

use memory::{CapabilityFlags, Layout, ManagerTrait, RegionDescriptor, RegionManager, Statistics};

/// Default threshold (in bytes) at or above which eligible allocations prefer PSRAM.
pub const DEFAULT_LARGE_ALLOCATION_THRESHOLD: usize = 4096;

/// Number of static memory regions managed on the selected chip.
///
/// Chips with external PSRAM support (`esp32`, `esp32s2`, `esp32s3`, `esp32c5`, `esp32c61`,
/// `esp32p4`, `esp32s31`) reserve slot `0` for internal SRAM and slot `1` for optional PSRAM.
/// ESP32 also reserves slot `2` for reclaimed internal SRAM.
/// Chips without PSRAM (`esp32c2`, `esp32c3`, `esp32c6`, `esp32h2`) allocate a single slot.
pub const REGION_COUNT: usize = if cfg!(feature = "esp32") {
    3
} else if cfg!(any(
    feature = "esp32s2",
    feature = "esp32s3",
    feature = "esp32c5",
    feature = "esp32c61",
    feature = "esp32p4",
    feature = "esp32s31",
)) {
    2
} else {
    1
};

/// Region reserved for external PSRAM on chips that support it.
pub const PSRAM_REGION: usize = 1;

#[cfg(feature = "esp32")]
const RECLAIMED_INTERNAL_REGION: usize = 2;

const INTERNAL_DMA_ALIGNMENT: usize = if cfg!(feature = "esp32p4") { 64 } else { 4 };

const INTERNAL_DESCRIPTOR: RegionDescriptor = RegionDescriptor::new(
    CapabilityFlags::DirectMemoryAccess.union(CapabilityFlags::AlwaysAccessible),
)
.with_dma_alignment(INTERNAL_DMA_ALIGNMENT);

#[cfg(any(
    feature = "esp32",
    feature = "esp32s2",
    feature = "esp32s3",
    feature = "esp32c5",
    feature = "esp32c61",
    feature = "esp32p4",
    feature = "esp32s31",
))]
const PSRAM_DESCRIPTOR: RegionDescriptor = {
    if cfg!(any(feature = "esp32", feature = "esp32s2")) {
        // ESP32: esp-hal does not enable dma_can_access_psram at all.
        // ESP32-S2: only SPI2, I2S0 and the crypto DMA can reach PSRAM
        // (`can_access_psram()` is false for SPI3 and copy DMA), and a
        // capability flag cannot say which channel will use the buffer.
        RegionDescriptor::new(CapabilityFlags::empty())
    } else if cfg!(feature = "esp32p4") {
        // ESP32-P4 PSRAM sits behind the L2 cache (up to 128-byte line size).
        RegionDescriptor::new(CapabilityFlags::DirectMemoryAccess).with_dma_alignment(128)
    } else {
        // ESP32-S3, S31, C5 and C61: 64 bytes covers the largest cache line
        // and the 64-byte external burst esp-hal allows on these chips
        // (inbound PSRAM transfers need alignment to the burst size).
        RegionDescriptor::new(CapabilityFlags::DirectMemoryAccess).with_dma_alignment(64)
    }
};

const fn default_descriptors() -> [RegionDescriptor; REGION_COUNT] {
    let descriptors = [INTERNAL_DESCRIPTOR; REGION_COUNT];
    #[cfg(any(
        feature = "esp32",
        feature = "esp32s2",
        feature = "esp32s3",
        feature = "esp32c5",
        feature = "esp32c61",
        feature = "esp32p4",
        feature = "esp32s31",
    ))]
    let descriptors = {
        let mut descriptors = descriptors;
        descriptors[PSRAM_REGION] = PSRAM_DESCRIPTOR;
        descriptors
    };
    descriptors
}

/// Espressif platform memory manager backing Xila's global allocator.
///
/// Region `0` is internal SRAM (`DirectMemoryAccess | AlwaysAccessible`). On PSRAM-capable chips,
/// region `1` is optional external PSRAM, left uninitialized until the board registers it via
/// [`Self::init_psram`] or [`Self::init_psram_from_hal`]. ESP32 uses region `2` for
/// optional reclaimed internal SRAM.
pub struct MemoryManager {
    inner: RegionManager<REGION_COUNT>,
    internal_only: AtomicBool,
}

impl Default for MemoryManager {
    fn default() -> Self {
        Self::new()
    }
}

impl MemoryManager {
    /// Creates an uninitialized [`MemoryManager`] using [`DEFAULT_LARGE_ALLOCATION_THRESHOLD`].
    pub const fn new() -> Self {
        Self::with_large_allocation_threshold(Some(DEFAULT_LARGE_ALLOCATION_THRESHOLD))
    }

    /// Creates an uninitialized [`MemoryManager`] with a custom large-allocation threshold.
    pub const fn with_large_allocation_threshold(threshold: Option<usize>) -> Self {
        Self {
            inner: RegionManager::new(default_descriptors(), threshold),
            internal_only: AtomicBool::new(false),
        }
    }

    /// Initializes the internal SRAM heap region `[start, start + size)`.
    ///
    /// # Safety
    ///
    /// `start` must point to an exclusive, writable, statically reserved SRAM range of at
    /// least `size` bytes. It must be DMA-capable data RAM accessible during flash cache
    /// suspension. The range must remain reserved for the lifetime of the firmware.
    pub unsafe fn init_internal(&self, start: *mut u8, size: usize) -> bool {
        unsafe { self.inner.init_region(0, start, size) }
    }

    /// Initializes the internal SRAM heap region from a static mutable slice.
    ///
    /// # Safety
    ///
    /// The slice must be in DMA-capable internal data RAM that stays accessible while the
    /// flash cache is suspended.
    pub unsafe fn init_internal_from_slice(&self, slice: &'static mut [MaybeUninit<u8>]) -> bool {
        self.inner.init_region_from_slice(0, slice)
    }

    /// Registers a second internal SRAM region reclaimed from the ESP-IDF bootloader.
    ///
    /// # Safety
    ///
    /// The application must own this region after bootloader handoff. It must
    /// be DMA-capable internal data RAM and remain accessible during flash
    /// cache suspension. The slice must not overlap another heap region.
    #[cfg(feature = "esp32")]
    pub unsafe fn init_reclaimed_internal(&self, slice: &'static mut [MaybeUninit<u8>]) -> bool {
        self.inner
            .init_region_from_slice(RECLAIMED_INTERNAL_REGION, slice)
    }

    /// Registers an optional external PSRAM region `[start, start + size)`.
    ///
    /// Returns `false` (leaving all allocations in internal SRAM) if `start` is null or `size` is
    /// too small, which allows callers to pass `(null_mut(), 0)` when PSRAM detection fails.
    ///
    /// # Safety
    ///
    /// If `start` is non-null, the range must be valid, mapped PSRAM reserved exclusively
    /// for this allocator for the lifetime of the firmware.
    #[cfg(any(
        feature = "esp32",
        feature = "esp32s2",
        feature = "esp32s3",
        feature = "esp32c5",
        feature = "esp32c61",
        feature = "esp32p4",
        feature = "esp32s31",
    ))]
    pub unsafe fn init_psram(&self, start: *mut u8, size: usize) -> bool {
        unsafe { self.inner.init_region(PSRAM_REGION, start, size) }
    }

    /// Registers the full external PSRAM region discovered by esp-hal.
    ///
    /// # Safety
    ///
    /// The mapped range must be reserved exclusively for this manager for the lifetime
    /// of the firmware. In particular, it cannot also be handed to another allocator
    /// or used for static data.
    #[cfg(any(
        feature = "esp32",
        feature = "esp32s2",
        feature = "esp32s3",
        feature = "esp32c5",
        feature = "esp32c61",
        feature = "esp32p4",
        feature = "esp32s31",
    ))]
    pub unsafe fn init_psram_from_hal(&self, psram: &esp_hal::psram::Psram) -> bool {
        let (start, size) = psram.raw_parts();
        unsafe { self.init_psram(start, size) }
    }

    /// While set, every allocation goes to internal SRAM, as if it asked for
    /// [`CapabilityFlags::AlwaysAccessible`], whatever its size. For buffers
    /// that another library allocates and DMA then reads, such as LVGL's draw
    /// buffers: internal SRAM needs no cache maintenance and keeps up with
    /// the peripheral. Allocations of other tasks meanwhile are affected too,
    /// so keep the window short.
    pub fn set_internal_only(&self, internal_only: bool) {
        self.internal_only.store(internal_only, Ordering::Release);
    }

    fn effective_capabilities(&self, capabilities: CapabilityFlags) -> CapabilityFlags {
        if self.internal_only.load(Ordering::Acquire) {
            capabilities | CapabilityFlags::AlwaysAccessible
        } else {
            capabilities
        }
    }

    /// Returns per-region and aggregate heap statistics.
    pub fn get_statistics(&self) -> Statistics<REGION_COUNT> {
        self.inner.get_statistics()
    }
}

fn allocation_capabilities(capabilities: CapabilityFlags) -> CapabilityFlags {
    #[cfg(feature = "wifi")]
    if capabilities.is_empty() && crate::wifi::internal_allocation_required() {
        return CapabilityFlags::AlwaysAccessible;
    }
    capabilities
}

impl ManagerTrait for MemoryManager {
    unsafe fn allocate(
        &self,
        capabilities: CapabilityFlags,
        layout: Layout,
    ) -> Option<NonNull<u8>> {
        let capabilities = self.effective_capabilities(capabilities);
        unsafe {
            self.inner
                .allocate(allocation_capabilities(capabilities), layout)
        }
    }

    unsafe fn deallocate(&self, pointer: NonNull<u8>, layout: Layout) {
        unsafe { self.inner.deallocate(pointer, layout) }
    }

    unsafe fn reallocate(
        &self,
        capabilities: CapabilityFlags,
        old_pointer: NonNull<u8>,
        old_layout: Layout,
        new_layout: Layout,
    ) -> Option<NonNull<u8>> {
        let capabilities = self.effective_capabilities(capabilities);
        unsafe {
            self.inner.reallocate(
                allocation_capabilities(capabilities),
                old_pointer,
                old_layout,
                new_layout,
            )
        }
    }

    fn get_used(&self) -> usize {
        self.inner.get_used()
    }

    fn get_free(&self) -> usize {
        self.inner.get_free()
    }

    fn get_total_size(&self) -> usize {
        self.inner.get_total_size()
    }
}

#[macro_export]
macro_rules! instantiate_global_allocator {
    ($manager:expr) => {
        $crate::memory_exported::instantiate_global_allocator!($manager);
    };
    () => {
        pub static __DRIVERS_ESPRESSIF_MEMORY_ALLOCATOR: $crate::memory::MemoryManager =
            $crate::memory::MemoryManager::new();

        $crate::memory_exported::instantiate_global_allocator!(
            &__DRIVERS_ESPRESSIF_MEMORY_ALLOCATOR
        );
    };
}

pub use instantiate_global_allocator;
