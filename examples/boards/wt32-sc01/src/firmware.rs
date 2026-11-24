//! Wireless-Tag WT32-SC01 with the ESP32-WROVER-B module.

mod screen;

use alloc::string::String;
use core::{cell::RefCell, mem::MaybeUninit, panic::PanicInfo};

use critical_section::Mutex;

use drivers_espressif::random::RandomDevice;
use esp_hal::{
    clock::CpuClock,
    psram::{Psram, PsramCacheSpeed, PsramConfig},
    timer::timg::TimerGroup,
    uart::{Config as UartConfiguration, Uart},
};
use esp_storage::FlashStorage;
use firmware::{Board, ExternalMemory, MemoryReport, StoragePartition, parse_decimal, session};
use static_cell::StaticCell;

esp_bootloader_esp_idf::esp_app_desc!();

const STORAGE: StoragePartition = StoragePartition {
    offset: parse_decimal(env!("XILA_STORAGE_OFFSET")) as u32,
    length: parse_decimal(env!("XILA_STORAGE_LENGTH")) as u32,
};

// The primary DRAM linker region is 192 KiB, shared by static data and the
// stack. Reserving 96 KiB here alongside the 96 KiB reclaimed DRAM2 region
// gives 192 KiB of DMA-capable internal heap while keeping more than 75 KiB of
// primary DRAM for the main stack (checked at link time against 40 KiB) and
// headroom for future Wi-Fi static allocations.
const INTERNAL_HEAP_SIZE: usize = 96 * 1024;
static INTERNAL_HEAP: StaticCell<[MaybeUninit<u8>; INTERNAL_HEAP_SIZE]> = StaticCell::new();

// esp-hal's ESP32 DRAM2 region excludes the ROM data and stacks. The ESP-IDF
// bootloader releases the region when it transfers control to the application.
const RECLAIMED_HEAP_SIZE: usize = 96 * 1024;
#[esp_hal::ram(reclaimed)]
static mut RECLAIMED_HEAP: [MaybeUninit<u8>; RECLAIMED_HEAP_SIZE] =
    [MaybeUninit::uninit(); RECLAIMED_HEAP_SIZE];

/// esp-hal starts the ESP32 at 80 MHz by default; run it at its 240 MHz
/// maximum. The display bus and UART take their clocks from the 80 MHz APB
/// clock, so their rates do not change.
const CPU_CLOCK: CpuClock = CpuClock::_240MHz;

#[esp_hal::main]
fn main() -> ! {
    let peripherals = esp_hal::init(esp_hal::Config::default().with_cpu_clock(CPU_CLOCK));
    let reservation = INTERNAL_HEAP.init_with(|| [MaybeUninit::uninit(); INTERNAL_HEAP_SIZE]);
    // SAFETY: the reservation is exclusive and linked into internal data SRAM.
    let heap = unsafe { firmware::initialize_heap_from_slice(reservation) };
    // SAFETY: the bootloader has handed over to the application; this
    // separate, DMA-capable reservation is passed to the heap only once.
    let reclaimed = unsafe {
        core::slice::from_raw_parts_mut((&raw mut RECLAIMED_HEAP).cast(), RECLAIMED_HEAP_SIZE)
    };
    assert!(
        // SAFETY: the reclaimed reservation is registered with the heap once.
        unsafe { heap.init_reclaimed_internal(reclaimed) },
        "Could not register reclaimed internal SRAM"
    );

    let timer_group = TimerGroup::new(peripherals.TIMG0);
    drivers_espressif::time::initialize(timer_group.timer0);

    let serial = Uart::new(peripherals.UART0, UartConfiguration::default())
        .expect("Failed to configure UART0")
        .with_tx(peripherals.GPIO1)
        .with_rx(peripherals.GPIO3);

    // Revision v0.0 has no supported PSRAM mapping. Later revisions need
    // three things before ordinary allocations may use PSRAM, as in ESP-IDF:
    // - the cache workaround of erratum CPU-3.9 in all code, Rust (the
    //   target's Rust flags) and C (`CFLAGS` of the board build);
    // - atomic operations that keep the compare-and-set instruction off
    //   PSRAM (`drivers_espressif::atomics`);
    // - internal SRAM for what hardware reads or what is used while the flash
    //   is written: DMA buffers (the display bus checks the address), the
    //   flash transfer buffer (on the stack) and the stacks themselves.
    let external = (esp_hal::efuse::chip_revision().major != 0).then(|| {
        let psram = Psram::new(
            peripherals.PSRAM,
            PsramConfig {
                cache_speed: PsramCacheSpeed::PsramCacheF40mS40m,
                ..PsramConfig::default()
            },
        );
        let (start, size) = psram.raw_parts();
        // SAFETY: the heap is the only owner of this PSRAM mapping.
        let registered = unsafe { heap.init_psram_from_hal(&psram) };
        ExternalMemory {
            start: start as usize,
            size,
            registered,
        }
    });

    let (display, report) = match screen::start(screen::Pins {
        spi: peripherals.SPI2,
        dma: peripherals.DMA_SPI2,
        data: peripherals.GPIO13,
        clock: peripherals.GPIO14,
        select: peripherals.GPIO15,
        command_data: peripherals.GPIO21,
        reset: peripherals.GPIO22,
        backlight: peripherals.GPIO23,
        i2c: peripherals.I2C0,
        touch_data: peripherals.GPIO18,
        touch_clock: peripherals.GPIO19,
    }) {
        Ok((display, identifiers)) => (Some(display), Ok(identifiers)),
        Err(error) => (None, Err(error)),
    };
    critical_section::with(|section| SCREEN_REPORT.borrow(section).replace(Some(report)));

    firmware::run(Board {
        name: "WT32-SC01",
        console: serial.into(),
        flash_storage: FlashStorage::new(peripherals.FLASH),
        storage: STORAGE,
        random_device: RandomDevice::new(peripherals.RNG, peripherals.ADC1),
        has_entropy_source: true,
        check_random_device: true,
        memory: MemoryReport {
            internal_sram_size: 520 * 1024,
            external,
        },
        memory_check: Some(check_external_memory),
        system_check: Some(log_screen_report),
        sessions: if display.is_some() {
            &[session::CONSOLE, session::GRAPHICAL]
        } else {
            &[session::CONSOLE]
        },
        display,
    })
}

type ScreenReport = Result<(u8, u8), String>;
static SCREEN_REPORT: Mutex<RefCell<Option<ScreenReport>>> = Mutex::new(RefCell::new(None));

fn log_screen_report() {
    match critical_section::with(|section| SCREEN_REPORT.borrow(section).take()) {
        Some(Ok((chip, vendor))) => xila::log::information!(
            "Xila: ST7796 on HSPI, FT6336U chip {chip:#04x}, vendor {vendor:#04x}"
        ),
        Some(Err(error)) => xila::log::error!("Xila: the screen is off, {error}"),
        None => {}
    }
}

fn check_external_memory(report: &MemoryReport) {
    if report.external.is_none() {
        xila::log::warning!("Xila: PSRAM unavailable on ESP32 revision v0.0");
    }
    firmware::psram_self_test::run(firmware::heap(), report);
    #[cfg(feature = "hardware-atomics-probe")]
    probe_hardware_atomics(report);
}

/// Reports what the compare-and-set instruction does on a word in PSRAM, for
/// the record: the firmware never uses it there. A working instruction
/// returns the old value both times and stores only the first time. This
/// single-threaded probe cannot show a lack of atomicity.
#[cfg(feature = "hardware-atomics-probe")]
fn probe_hardware_atomics(report: &MemoryReport) {
    use alloc::vec::Vec;
    use drivers_espressif::atomics::xila_esp32_hardware_compare_and_set;

    let Some(external) = report.external.filter(|external| external.registered) else {
        return;
    };
    let mut words: Vec<u32> = Vec::new();
    if words.try_reserve_exact(4096).is_err() {
        xila::log::error!("Xila: hardware compare-and-set probe could not allocate");
        return;
    }
    words.push(5);
    let word = words.as_mut_ptr();
    if !(external.start..external.start + external.size).contains(&(word as usize)) {
        xila::log::warning!("Xila: hardware compare-and-set probe skipped: not in PSRAM");
        return;
    }
    xila::log::information!("Xila: probing the hardware compare-and-set on PSRAM at {word:p}");
    // SAFETY: the word is valid, aligned and exclusive. It is in PSRAM on
    // purpose: the result is only reported.
    let (matched, stored, mismatched, kept) = unsafe {
        let matched = xila_esp32_hardware_compare_and_set(word, 5, 9);
        let stored = word.read_volatile();
        let mismatched = xila_esp32_hardware_compare_and_set(word, 5, 13);
        (matched, stored, mismatched, word.read_volatile())
    };
    xila::log::information!(
        "Xila: hardware compare-and-set on PSRAM: a match returned {matched} and left {stored} \
         (5 and 9 when it works), a mismatch returned {mismatched} and left {kept} (9 and 9)"
    );
}

#[panic_handler]
fn panic(information: &PanicInfo<'_>) -> ! {
    esp_println::println!("Xila panic: {information}");
    loop {
        core::hint::spin_loop();
    }
}
