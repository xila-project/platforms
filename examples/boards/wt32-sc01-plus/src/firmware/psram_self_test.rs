//! The PSRAM self-test: an ordinary large allocation, from the allocator down
//! to the chip.

use core::{alloc::Layout, ops::Range, ptr::NonNull};

use drivers_espressif::memory::MemoryManager;
use firmware::MemoryReport;
use xila::{
    log,
    memory::{CapabilityFlags, ManagerTrait},
};

/// Runs [`check`] on the board's PSRAM, as its memory check, when the heap
/// uses it.
pub(super) fn run(report: &MemoryReport) {
    if let Some(external) = report.external
        && external.registered
    {
        check(
            firmware::heap(),
            external.start..external.start + external.size,
        );
    }
}

/// Size of the buffer that [`run`] checks.
const SELF_TEST_SIZE: usize = 1024 * 1024;

/// The pattern word at `address`: it differs between neighbouring words and
/// between the two halves of the address.
const fn pattern(address: usize) -> u32 {
    let address = address as u32;
    address ^ address.rotate_left(16) ^ 0xa5a5_5a5a
}

/// The first word of `words` that does not hold `expected(address)`, as its
/// address, the expected and the read value.
fn first_mismatch(
    words: NonNull<u32>,
    count: usize,
    expected: impl Fn(usize) -> u32,
) -> Option<(usize, u32, u32)> {
    (0..count).find_map(|index| {
        // SAFETY: `index` is below the word count of the live allocation.
        let word = unsafe { words.add(index) };
        let address = word.as_ptr() as usize;
        // SAFETY: the word is aligned and inside the allocation.
        let value = unsafe { word.as_ptr().read_volatile() };
        let wanted = expected(address);
        (value != wanted).then_some((address, wanted, value))
    })
}

fn fill(words: NonNull<u32>, count: usize, value: impl Fn(usize) -> u32) {
    for index in 0..count {
        // SAFETY: `index` is below the word count of the live allocation.
        let word = unsafe { words.add(index) };
        // SAFETY: the word is aligned and inside the allocation.
        unsafe { word.as_ptr().write_volatile(value(word.as_ptr() as usize)) };
    }
}

/// Region 1 of the heap, the external PSRAM.
const PSRAM_REGION: usize = 1;

/// Logs the PSRAM region of the heap at `stage` and returns its used bytes.
fn log_psram_region(heap: &MemoryManager, stage: &str) -> usize {
    match &heap.get_statistics().region_stats[PSRAM_REGION] {
        Some(region) => {
            log::information!(
                "Xila: PSRAM self-test, heap region {PSRAM_REGION} {stage}: \
                 {} bytes used, {} free, {} total",
                region.used,
                region.free,
                region.size
            );
            region.used
        }
        None => {
            log::error!("Xila: PSRAM self-test, heap region {PSRAM_REGION} {stage}: absent");
            0
        }
    }
}

/// Checks the whole path from an ordinary allocation to the PSRAM chip.
///
/// The buffer comes from the heap with no special capability, as any large
/// allocation would: its size is above the large-allocation threshold, so the
/// heap should place it in the PSRAM region. The test checks that the address
/// lies in the mapped PSRAM range, that the region's used bytes grew by at
/// least the buffer size, and that the memory keeps an address-dependent
/// pattern and its inverse. The buffer is larger than the data cache, so most
/// of it is read back from the chip, not from the cache. Logs the region
/// before, during and after, then the result.
fn check(heap: &MemoryManager, psram: Range<usize>) {
    let used_before = log_psram_region(heap, "before");
    let layout = Layout::from_size_align(SELF_TEST_SIZE, 64).expect("valid self-test layout");
    // SAFETY: the layout has a non-zero size.
    let Some(buffer) = (unsafe { heap.allocate(CapabilityFlags::empty(), layout) }) else {
        log::error!("Xila: PSRAM self-test could not allocate {SELF_TEST_SIZE} bytes");
        return;
    };
    let used_during = log_psram_region(heap, "during");
    let start = buffer.as_ptr() as usize;
    let end = start + SELF_TEST_SIZE;
    let failure = if start < psram.start || end > psram.end {
        log::error!(
            "Xila: PSRAM self-test buffer at {start:#010x}..{end:#010x} is outside \
             the PSRAM range {:#010x}..{:#010x}",
            psram.start,
            psram.end
        );
        true
    } else if used_during < used_before + SELF_TEST_SIZE {
        log::error!(
            "Xila: PSRAM self-test buffer at {start:#010x} is in the PSRAM range, but heap \
             region {PSRAM_REGION} grew by {} bytes only",
            used_during.saturating_sub(used_before)
        );
        true
    } else {
        let words = buffer.cast::<u32>();
        let count = SELF_TEST_SIZE / size_of::<u32>();
        fill(words, count, pattern);
        let mismatch = first_mismatch(words, count, pattern).or_else(|| {
            fill(words, count, |address| !pattern(address));
            first_mismatch(words, count, |address| !pattern(address))
        });
        match mismatch {
            None => false,
            Some((address, expected, value)) => {
                log::error!(
                    "Xila: PSRAM self-test failed at {address:#010x}: \
                     wrote {expected:#010x}, read {value:#010x}"
                );
                true
            }
        }
    };
    // SAFETY: `buffer` came from this heap with this layout and is not used again.
    unsafe { heap.deallocate(buffer, layout) };
    let used_after = log_psram_region(heap, "after");
    if used_after != used_before {
        log::warning!(
            "Xila: PSRAM self-test, heap region {PSRAM_REGION} holds {used_after} bytes \
             after the buffer was freed, {used_before} before"
        );
    }
    if !failure {
        log::information!("Xila: PSRAM self-test passed ({SELF_TEST_SIZE} bytes at {start:#010x})");
    }
}
