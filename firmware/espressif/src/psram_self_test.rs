//! The PSRAM self-test: an ordinary large allocation, from the allocator down
//! to the chip, then the atomic operations, fills and copies, and a shared
//! allocation in PSRAM.

use alloc::{sync::Arc, vec::Vec};
use core::{
    alloc::Layout,
    hint::black_box,
    ops::Range,
    ptr::NonNull,
    sync::atomic::{AtomicBool, AtomicU8, AtomicU16, AtomicU32, Ordering},
};

use drivers_espressif::memory::{MemoryManager, PSRAM_REGION};
use xila::{
    log,
    memory::{CapabilityFlags, ManagerTrait},
};

/// Tests the PSRAM mapping when the board registered it with `heap`.
pub fn run(heap: &MemoryManager, report: &crate::MemoryReport) {
    if let Some(external) = report.external
        && external.registered
    {
        let psram = external.start..external.start + external.size;
        check(heap, psram.clone());
        check_atomics(heap, psram.clone());
        check_copies(heap, psram.clone());
        check_shared_allocation(psram);
    }
}

/// Size of the buffer that [`check`] checks.
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

/// Logs the PSRAM region of the heap at `stage` and returns its used bytes.
fn log_psram_region(heap: &MemoryManager, stage: &str) -> usize {
    match heap
        .get_statistics()
        .region_stats
        .get(PSRAM_REGION)
        .and_then(Option::as_ref)
    {
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

/// Size of the buffer that holds the atomic values: above the
/// large-allocation threshold, so the heap places it in PSRAM.
const ATOMICS_SIZE: usize = 8 * 1024;

/// How many times the atomic counter is incremented.
const INCREMENTS: u32 = 1000;

/// Runs each kind of atomic operation on values in the PSRAM `words` and
/// returns the first one that gave a wrong result.
///
/// # Safety
///
/// `words` must point to at least three writable, aligned words that nothing
/// else uses.
unsafe fn first_atomic_failure(words: NonNull<u32>) -> Option<&'static str> {
    // The compiler must not see through the pointer and compute the results.
    let words = black_box(words.as_ptr());
    // SAFETY: the caller provides three exclusive, aligned words; the atomic
    // types below borrow distinct parts of them.
    let (counter, bytes, halves, flag) = unsafe {
        words.write_bytes(0, 3);
        let bytes = words.add(1).cast::<u8>();
        let halves = words.add(2).cast::<u16>();
        (
            AtomicU32::from_ptr(words),
            [
                AtomicU8::from_ptr(bytes),
                AtomicU8::from_ptr(bytes.add(1)),
                AtomicU8::from_ptr(bytes.add(2)),
            ],
            [
                AtomicU16::from_ptr(halves),
                AtomicU16::from_ptr(halves.add(1)),
            ],
            AtomicBool::from_ptr(bytes.add(3).cast::<bool>()),
        )
    };

    for _ in 0..INCREMENTS {
        counter.fetch_add(1, Ordering::SeqCst);
    }
    if counter.load(Ordering::SeqCst) != INCREMENTS {
        return Some("fetch_add");
    }
    if counter.compare_exchange(INCREMENTS, 7, Ordering::SeqCst, Ordering::SeqCst) != Ok(INCREMENTS)
        || counter.load(Ordering::SeqCst) != 7
    {
        return Some("compare_exchange on a match");
    }
    if counter.compare_exchange(INCREMENTS, 9, Ordering::SeqCst, Ordering::SeqCst) != Err(7)
        || counter.load(Ordering::SeqCst) != 7
    {
        return Some("compare_exchange on a mismatch");
    }
    if counter.swap(0xf0f0_00ff, Ordering::SeqCst) != 7 {
        return Some("swap");
    }
    if counter.fetch_and(0x0ff0_0ff0, Ordering::SeqCst) != 0xf0f0_00ff
        || counter.fetch_or(0x0000_000f, Ordering::SeqCst) != 0x00f0_00f0
        || counter.fetch_xor(0xffff_ffff, Ordering::SeqCst) != 0x00f0_00ff
        || counter.fetch_sub(1, Ordering::SeqCst) != 0xff0f_ff00
        || counter.fetch_max(5, Ordering::SeqCst) != 0xff0f_feff
        || counter.fetch_min(5, Ordering::SeqCst) != 0xff0f_feff
        || counter.load(Ordering::SeqCst) != 5
    {
        return Some("a 32-bit read-modify-write");
    }

    // Values smaller than a word share it with their neighbours.
    bytes[0].store(0x11, Ordering::SeqCst);
    bytes[1].store(0xff, Ordering::SeqCst);
    bytes[2].store(0x33, Ordering::SeqCst);
    if bytes[1].fetch_add(2, Ordering::SeqCst) != 0xff
        || bytes[1].load(Ordering::SeqCst) != 1
        || bytes[0].load(Ordering::SeqCst) != 0x11
        || bytes[2].load(Ordering::SeqCst) != 0x33
    {
        return Some("an 8-bit fetch_add");
    }
    if flag.swap(true, Ordering::SeqCst)
        || flag.compare_exchange(true, false, Ordering::SeqCst, Ordering::SeqCst) != Ok(true)
        || flag.compare_exchange(true, false, Ordering::SeqCst, Ordering::SeqCst) != Err(false)
        || bytes[2].load(Ordering::SeqCst) != 0x33
    {
        return Some("an 8-bit swap or compare_exchange");
    }
    halves[0].store(0x1234, Ordering::SeqCst);
    halves[1].store(0xffff, Ordering::SeqCst);
    if halves[1].fetch_add(1, Ordering::SeqCst) != 0xffff
        || halves[0].fetch_or(0x8000, Ordering::SeqCst) != 0x1234
        || halves[0].load(Ordering::SeqCst) != 0x9234
        || halves[1].load(Ordering::SeqCst) != 0
    {
        return Some("a 16-bit read-modify-write");
    }
    None
}

/// Checks the atomic operations on values in PSRAM.
///
/// Reference counts, locks and flags of ordinary allocations can be in PSRAM,
/// where the ESP32 must not use its compare-and-set instruction: the firmware
/// then provides the operations (`drivers_espressif::atomics`). The test runs
/// each kind of operation, on 32, 16 and 8-bit values, and checks that a
/// value smaller than a word leaves its neighbours alone.
fn check_atomics(heap: &MemoryManager, psram: Range<usize>) {
    let layout = Layout::from_size_align(ATOMICS_SIZE, 64).expect("valid self-test layout");
    // SAFETY: the layout has a non-zero size.
    let Some(buffer) = (unsafe { heap.allocate(CapabilityFlags::empty(), layout) }) else {
        log::error!("Xila: PSRAM atomics self-test could not allocate {ATOMICS_SIZE} bytes");
        return;
    };
    let start = buffer.as_ptr() as usize;
    if !psram.contains(&start) {
        log::error!(
            "Xila: PSRAM atomics self-test buffer at {start:#010x} is outside the PSRAM range"
        );
    } else {
        // SAFETY: the buffer is larger than three words, aligned and exclusive.
        match unsafe { first_atomic_failure(buffer.cast()) } {
            None => log::information!("Xila: PSRAM atomics self-test passed (at {start:#010x})"),
            Some(operation) => {
                log::error!("Xila: PSRAM atomics self-test failed: {operation} at {start:#010x}")
            }
        }
    }
    // SAFETY: `buffer` came from this heap with this layout and is not used again.
    unsafe { heap.deallocate(buffer, layout) };
}

/// Size of the buffer that [`check_copies`] fills and copies.
const COPY_SIZE: usize = 8 * 1024;

/// Fills and copies bytes of the PSRAM `buffer` of [`COPY_SIZE`] bytes and
/// returns the first operation that left wrong contents.
///
/// # Safety
///
/// `buffer` must point to [`COPY_SIZE`] writable bytes that nothing else uses.
unsafe fn first_copy_failure(buffer: NonNull<u8>) -> Option<&'static str> {
    const PATTERN_SIZE: usize = 251;
    let pattern: [u8; PATTERN_SIZE] = core::array::from_fn(|index| index as u8 ^ 0x5a);
    let buffer = black_box(buffer.as_ptr());
    // SAFETY: the caller provides `COPY_SIZE` exclusive bytes.
    let bytes = unsafe { core::slice::from_raw_parts_mut(buffer, COPY_SIZE) };

    // `black_box` makes the compiler read the memory back for each check
    // instead of assuming what the operation before it wrote.

    // An odd start and length exercise the unaligned ends of each loop.
    bytes.fill(0);
    bytes[3..COPY_SIZE - 5].fill(0xa5);
    let bytes = black_box(bytes);
    if bytes[..3].iter().any(|byte| *byte != 0)
        || bytes[3..COPY_SIZE - 5].iter().any(|byte| *byte != 0xa5)
        || bytes[COPY_SIZE - 5..].iter().any(|byte| *byte != 0)
    {
        return Some("a fill");
    }
    // Each chunk starts at a different alignment.
    for chunk in bytes.chunks_mut(PATTERN_SIZE) {
        chunk.copy_from_slice(&pattern[..chunk.len()]);
    }
    let bytes = black_box(bytes);
    if bytes
        .chunks(PATTERN_SIZE)
        .any(|chunk| chunk != &pattern[..chunk.len()])
    {
        return Some("a copy from internal memory");
    }
    // Overlapping moves inside PSRAM, in both directions.
    bytes.copy_within(0..PATTERN_SIZE, 7);
    let bytes = black_box(bytes);
    if bytes[7..7 + PATTERN_SIZE] != pattern {
        return Some("an overlapping move to a higher address");
    }
    bytes.copy_within(7..7 + PATTERN_SIZE, 2);
    let bytes = black_box(bytes);
    if bytes[2..2 + PATTERN_SIZE] != pattern {
        return Some("an overlapping move to a lower address");
    }
    None
}

/// Checks `memset`, `memcpy` and `memmove` on PSRAM: fills, copies from
/// internal memory and overlapping moves, at unaligned addresses and lengths.
/// On the ESP32 these run the firmware's own loops, not the ROM routines
/// (`drivers_espressif::rom_memory`).
fn check_copies(heap: &MemoryManager, psram: Range<usize>) {
    let layout = Layout::from_size_align(COPY_SIZE, 64).expect("valid self-test layout");
    // SAFETY: the layout has a non-zero size.
    let Some(buffer) = (unsafe { heap.allocate(CapabilityFlags::empty(), layout) }) else {
        log::error!("Xila: PSRAM copy self-test could not allocate {COPY_SIZE} bytes");
        return;
    };
    let start = buffer.as_ptr() as usize;
    if !psram.contains(&start) {
        log::error!(
            "Xila: PSRAM copy self-test buffer at {start:#010x} is outside the PSRAM range"
        );
    } else {
        // SAFETY: the buffer holds `COPY_SIZE` exclusive bytes.
        match unsafe { first_copy_failure(buffer) } {
            None => log::information!("Xila: PSRAM copy self-test passed (at {start:#010x})"),
            Some(operation) => {
                log::error!("Xila: PSRAM copy self-test failed: {operation} at {start:#010x}")
            }
        }
    }
    // SAFETY: `buffer` came from this heap with this layout and is not used again.
    unsafe { heap.deallocate(buffer, layout) };
}

/// Size of the shared allocation that [`check_shared_allocation`] makes.
const SHARED_SIZE: usize = 16 * 1024;

/// How many references [`check_shared_allocation`] takes.
const REFERENCES: usize = 100;

/// Checks a reference-counted allocation that the global allocator places in
/// PSRAM: its two counters are at the start of the allocation, so they are in
/// PSRAM too. This is the ordinary path of every large `Arc` in the system.
fn check_shared_allocation(psram: Range<usize>) {
    let mut bytes = Vec::new();
    if bytes.try_reserve_exact(SHARED_SIZE).is_err() {
        log::error!("Xila: PSRAM shared allocation self-test could not allocate");
        return;
    }
    bytes.extend((0..SHARED_SIZE).map(|index| index as u8));
    let shared: Arc<[u8]> = Arc::from(bytes);
    let start = Arc::as_ptr(&shared).cast::<u8>() as usize;
    if !psram.contains(&start) {
        // Not an error: PSRAM may be full. The atomic operations were
        // checked above either way.
        log::warning!(
            "Xila: PSRAM shared allocation self-test skipped: the allocation is at {start:#010x}"
        );
        return;
    }
    let references: Vec<Arc<[u8]>> = (0..REFERENCES).map(|_| Arc::clone(&shared)).collect();
    let counted = Arc::strong_count(&shared);
    drop(references);
    let remaining = Arc::strong_count(&shared);
    let intact = shared
        .iter()
        .enumerate()
        .all(|(index, byte)| *byte == index as u8);
    if counted == REFERENCES + 1 && remaining == 1 && intact {
        log::information!(
            "Xila: PSRAM shared allocation self-test passed \
             ({REFERENCES} references to {SHARED_SIZE} bytes at {start:#010x})"
        );
    } else {
        log::error!(
            "Xila: PSRAM shared allocation self-test failed at {start:#010x}: {counted} \
             references counted ({} expected), {remaining} after release, data intact: {intact}",
            REFERENCES + 1
        );
    }
}
