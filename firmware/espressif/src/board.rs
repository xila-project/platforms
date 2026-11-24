//! The board description that a board crate hands to [`crate::run`].

use drivers_espressif::{console::ConsolePort, random::RandomDevice};
use esp_storage::FlashStorage;
use xila::file_system::DirectCharacterDevice;

use crate::session::Session;

/// Everything the firmware needs to know about a board. The board crate
/// initializes the peripherals and moves them in here.
pub struct Board {
    /// The board's name, logged at boot.
    pub name: &'static str,
    /// The console: a UART or the USB Serial/JTAG controller.
    pub console: ConsolePort,
    /// The flash chip that holds the storage partition.
    pub flash_storage: FlashStorage<'static>,
    /// The LittleFS partition, from the board's partition table.
    pub storage: StoragePartition,
    /// `/devices/random/source`.
    pub random_device: RandomDevice,
    /// Whether `random_device` is a secure entropy source. Without one, the
    /// firmware does not create the default account.
    pub has_entropy_source: bool,
    /// Whether to check at boot that the random device returns changing values.
    pub check_random_device: bool,
    /// The memory the board gave the heap, for the boot report.
    pub memory: MemoryReport,
    /// A board check that runs after the memory report, for example a PSRAM
    /// or executable memory self-test.
    pub memory_check: Option<fn(&MemoryReport)>,
    /// A board check that runs once the executables are mounted, before the
    /// heap report, for example a test frame on an emulated display.
    pub system_check: Option<fn()>,
    /// The screen, on boards that have one. The firmware drives it when it is
    /// built with the `display` feature.
    pub display: Option<Display>,
    /// The sessions to start, each on its own terminal.
    pub sessions: &'static [Session],
}

/// Asynchronous flush hooks for a screen whose bus can stream one LVGL draw
/// buffer over DMA while LVGL renders into the second draw buffer.
#[cfg(feature = "display")]
pub trait AsynchronousFlush: Send + Sync {
    /// Starts flushing `data` (already in the panel's wire pixel format) to
    /// `area`. Returns `true` when an asynchronous transfer is now in flight
    /// and must be completed by [`wait_for_flush`](Self::wait_for_flush) before
    /// `data` is reused, or `false` when the transfer completed synchronously.
    ///
    /// # Safety
    ///
    /// When this returns `true`, the memory behind `data` must stay valid and
    /// unmodified until [`wait_for_flush`](Self::wait_for_flush) returns.
    unsafe fn start_flush(&self, area: xila::graphics::Area, data: &[u8]) -> bool;

    /// Waits for any in-flight flush started by [`start_flush`](Self::start_flush)
    /// to complete.
    fn wait_for_flush(&self);
}

/// A screen for Xila's graphics, with its optional pointer.
pub struct Display {
    /// The screen device: resolution, drawing area and pixel writes.
    pub screen: &'static dyn DirectCharacterDevice,
    /// A pointer, such as a touch panel, that LVGL polls.
    pub pointer: Option<&'static dyn DirectCharacterDevice>,
    /// Size of each LVGL draw buffer in pixels: LVGL renders in partial mode,
    /// one area of at most this many pixels per flush.
    pub draw_buffer_pixels: usize,
    /// Whether LVGL renders into one buffer while the other is flushed.
    pub double_buffered: bool,
    /// Whether the screen takes RGB565 with the high byte first, as 8-bit
    /// 8080 and SPI panels do: LVGL then renders in that byte order.
    pub byte_swapped_rgb565: bool,
    /// Whether to keep the draw buffers in internal SRAM, where DMA reads them
    /// without cache maintenance and fast enough for the panel bus.
    pub internal_draw_buffers: bool,
    /// Optional asynchronous flush driver. When present, LVGL starts flushes
    /// here and waits in its `flush_wait_cb` so rendering overlaps DMA.
    #[cfg(feature = "display")]
    pub asynchronous_flush: Option<&'static dyn AsynchronousFlush>,
    /// Optional LVGL display refresh and pointer polling period in
    /// milliseconds. When `None`, LVGL keeps `LV_DEF_REFR_PERIOD` (33 ms).
    pub refresh_period_milliseconds: Option<u32>,
    /// Round invalidated areas to an even width so RGB565 writes are always
    /// 4-byte multiples, keeping ESP32 SPI DMA on its zero-copy path.
    pub align_area_width_even: bool,
    /// Whether the theme renders in its light mode, for a screen that takes
    /// too long to redraw to follow animations: state changes and animated
    /// scrolls then take one frame, and modal backdrops are not blended.
    pub light_rendering: bool,
}

/// The memory the board gave the heap.
#[derive(Clone, Copy, Debug)]
pub struct MemoryReport {
    /// The chip's internal SRAM, of which [`crate::HEAP_SIZE`] is reserved for
    /// the heap; the rest holds the stacks, static data and code that runs from
    /// RAM.
    pub internal_sram_size: usize,
    /// The external PSRAM, on boards that set it up, even when none was found.
    pub external: Option<ExternalMemory>,
}

/// External PSRAM as esp-hal mapped it.
#[derive(Clone, Copy, Debug)]
pub struct ExternalMemory {
    /// Start address of the mapping.
    pub start: usize,
    /// Size of the mapping in bytes; zero when no PSRAM was found.
    pub size: usize,
    /// Whether the heap uses it.
    pub registered: bool,
}

/// A data partition in the flash, in bytes.
#[derive(Clone, Copy, Debug)]
pub struct StoragePartition {
    pub offset: u32,
    pub length: u32,
}

/// Parses a decimal number at compile time, for the values that build scripts
/// pass through the environment. Fails the build on anything else.
pub const fn parse_decimal(text: &str) -> u64 {
    let digits = text.as_bytes();
    assert!(!digits.is_empty(), "expected a decimal number");
    let mut value: u64 = 0;
    let mut index = 0;
    while index < digits.len() {
        let digit = digits[index];
        assert!(digit.is_ascii_digit(), "expected a decimal number");
        value = value * 10 + (digit - b'0') as u64;
        index += 1;
    }
    value
}
