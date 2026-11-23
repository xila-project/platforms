//! The opt-in boot benchmark (`boot-benchmark` Cargo feature): a few timings,
//! logged once at boot at information level, to compare builds of this board
//! (CPU clock, flash mode and clock, PSRAM clock) on the hardware.
//!
//! - CPU: a register-only integer loop, which neither memory slows down.
//! - Flash: reads a fixed, initialized 256 KiB read-only buffer mapped from
//!   flash through the data cache, larger than the cache and cold before the
//!   read, so every cache line comes from the chip in the boot loader's read
//!   mode, which it logs too.
//! - PSRAM: writes then reads a heap buffer that lands in PSRAM, larger than
//!   the cache, as the LVGL draw buffers and WebAssembly memories do.

use core::{alloc::Layout, hint::black_box, ptr::addr_of};

use esp_hal::time::{Duration, Instant};
use firmware::MemoryReport;
use xila::{
    log,
    memory::{CapabilityFlags, ManagerTrait},
};

/// Iterations of the CPU loop: a few milliseconds at 240 MHz.
const CPU_ITERATIONS: u32 = 1_000_000;

/// Bytes of flash to read: four times the largest ESP32-S3 data cache (64 KiB)
/// and eight times the default 32 KiB cache.
const FLASH_READ_SIZE: usize = 256 * 1024;

/// Bytes of PSRAM to write and read: eight times the largest data cache.
const PSRAM_SIZE: usize = 512 * 1024;

/// `SPI_MEM_CTRL_REG(0)`: the read mode of the memory SPI controller that the
/// cache uses for flash (ESP32-S3 Technical Reference Manual, SPI0 at
/// 0x6000_3000, register offset 0x8).
const SPI0_CONTROL_REGISTER: *const u32 = 0x6000_3008 as *const u32;

/// One 64-byte cache line of non-trivial words repeated to fill
/// [`FLASH_BENCHMARK_BUFFER`].
const FLASH_BENCHMARK_LINE: [u32; 16] = [
    0x243f_6a88,
    0x85a3_08d3,
    0x1319_8a2e,
    0x0370_7344,
    0xa409_3822,
    0x299f_31d0,
    0x082e_fa98,
    0xec4e_6c89,
    0x4528_21e6,
    0x38d0_1377,
    0xbe54_66cf,
    0x34e9_0c6c,
    0xc0ac_29b7,
    0xc97c_50dd,
    0x3f84_d5b5,
    0xb547_0917,
];

/// Fixed, initialized read-only buffer mapped from flash, aligned to the
/// largest ESP32-S3 data cache line (64 bytes) so cache alignment stays
/// identical across builds.
#[repr(C, align(64))]
struct FlashBenchmarkBuffer([[u32; 16]; FLASH_READ_SIZE / 64]);

static FLASH_BENCHMARK_BUFFER: FlashBenchmarkBuffer =
    FlashBenchmarkBuffer([FLASH_BENCHMARK_LINE; FLASH_READ_SIZE / 64]);

/// Runs the benchmark and logs its results.
pub(super) fn run(report: &MemoryReport) {
    log::information!(
        "Xila: boot benchmark, CPU at {} MHz, flash at {} MHz in {} mode, PSRAM at {} MHz",
        esp_hal::clock::cpu_clock().as_mhz(),
        super::FLASH_FREQUENCY as u32,
        flash_read_mode(),
        super::PSRAM_FREQUENCY as u32
    );
    let cpu = cpu_loop();
    log::information!(
        "Xila: boot benchmark, CPU loop of {CPU_ITERATIONS} iterations in {} µs",
        cpu.as_micros()
    );
    let flash = flash_read();
    log::information!(
        "Xila: boot benchmark, flash read {FLASH_READ_SIZE} bytes in {} µs ({} MB/s)",
        flash.as_micros(),
        megabytes_per_second(FLASH_READ_SIZE, flash)
    );
    if report.external.is_some_and(|external| external.registered) {
        psram_write_read();
    }
}

/// The flash read mode the boot loader left in the memory SPI controller.
fn flash_read_mode() -> &'static str {
    // SAFETY: the register is always mapped, and reading it has no effect.
    let control = unsafe { SPI0_CONTROL_REGISTER.read_volatile() };
    let bit = |index: u32| control & (1 << index) != 0;
    if bit(24) {
        "QIO"
    } else if bit(20) {
        "QOUT"
    } else if bit(23) {
        "DIO"
    } else if bit(14) {
        "DOUT"
    } else {
        "slow"
    }
}

/// Bytes per microsecond, which is megabytes per second.
fn megabytes_per_second(bytes: usize, duration: Duration) -> u64 {
    bytes as u64 / duration.as_micros().max(1)
}

/// Times a xorshift loop whose state stays in registers.
fn cpu_loop() -> Duration {
    let start = Instant::now();
    let mut state = black_box(0x2545_f491_u32);
    for _ in 0..black_box(CPU_ITERATIONS) {
        state ^= state << 13;
        state ^= state >> 17;
        state ^= state << 5;
    }
    black_box(state);
    start.elapsed()
}

/// Sums every word of [`FLASH_BENCHMARK_BUFFER`] and returns the elapsed time.
///
/// Nothing accesses the buffer before [`run`], and [`super::psram_self_test`]
/// streams 1 MiB through the data cache immediately beforehand, so every cache
/// line is cold and fetched from flash.
fn flash_read() -> Duration {
    let start = black_box(addr_of!(FLASH_BENCHMARK_BUFFER).cast::<u32>());
    let count = FLASH_READ_SIZE / size_of::<u32>();
    let began = Instant::now();
    let mut sum = 0u32;
    for index in 0..count {
        // SAFETY: `index` is below the word count of `FLASH_BENCHMARK_BUFFER`,
        // which is static, mapped in `.rodata` and aligned to 64 bytes.
        sum = sum.wrapping_add(unsafe { start.add(index).read_volatile() });
    }
    black_box(sum);
    began.elapsed()
}

/// Times a write, then a read, of a heap buffer of [`PSRAM_SIZE`] bytes, and
/// logs both.
fn psram_write_read() {
    let heap = firmware::heap();
    let layout = Layout::from_size_align(PSRAM_SIZE, 64).expect("valid benchmark layout");
    // SAFETY: the layout has a non-zero size.
    let Some(buffer) = (unsafe { heap.allocate(CapabilityFlags::empty(), layout) }) else {
        log::warning!("Xila: boot benchmark could not allocate {PSRAM_SIZE} bytes of PSRAM");
        return;
    };
    let words = buffer.cast::<u32>();
    let count = PSRAM_SIZE / size_of::<u32>();

    let began = Instant::now();
    for index in 0..count {
        // SAFETY: `index` is below the word count of the live allocation.
        unsafe { words.add(index).as_ptr().write_volatile(index as u32) };
    }
    let write = began.elapsed();

    let began = Instant::now();
    let mut sum = 0u32;
    for index in 0..count {
        // SAFETY: as above; every word was written.
        sum = sum.wrapping_add(unsafe { words.add(index).as_ptr().read_volatile() });
    }
    black_box(sum);
    let read = began.elapsed();

    // SAFETY: `buffer` came from this heap with this layout and is not used again.
    unsafe { heap.deallocate(buffer, layout) };
    log::information!(
        "Xila: boot benchmark, PSRAM wrote {PSRAM_SIZE} bytes in {} µs ({} MB/s), \
         read them in {} µs ({} MB/s)",
        write.as_micros(),
        megabytes_per_second(PSRAM_SIZE, write),
        read.as_micros(),
        megabytes_per_second(PSRAM_SIZE, read)
    );
}
