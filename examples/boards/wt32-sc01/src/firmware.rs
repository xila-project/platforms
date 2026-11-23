//! Wireless-Tag WT32-SC01 with the ESP32-WROVER-B module.

mod screen;

use alloc::string::String;
use core::{cell::RefCell, mem::MaybeUninit, panic::PanicInfo};

use critical_section::Mutex;

use drivers_espressif::{memory::MemoryManager, random::RandomDevice};
use esp_hal::{
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

// The primary DRAM linker region is 192 KiB, shared by static data and the stack.
const INTERNAL_HEAP_SIZE: usize = 64 * 1024;
static INTERNAL_HEAP: StaticCell<[MaybeUninit<u8>; INTERNAL_HEAP_SIZE]> = StaticCell::new();

// esp-hal's ESP32 DRAM2 region excludes the ROM data and stacks. The ESP-IDF
// bootloader releases the region when it transfers control to the application.
const RECLAIMED_HEAP_SIZE: usize = 96 * 1024;
#[esp_hal::ram(reclaimed)]
static mut RECLAIMED_HEAP: [MaybeUninit<u8>; RECLAIMED_HEAP_SIZE] =
    [MaybeUninit::uninit(); RECLAIMED_HEAP_SIZE];

// Keep external storage explicit. ESP32 hardware atomics and flash ROM calls
// cannot use PSRAM buffers. LittleFS's C code is built without the PSRAM
// cache workaround and is safe because the global allocator serves internal
// SRAM only.
static EXTERNAL_HEAP: MemoryManager = MemoryManager::new();

/// Explicit access to the external data heap for CPU-only Rust buffers.
/// Callers must keep LittleFS, DMA and hardware atomics off this heap.
pub(crate) fn external_heap() -> &'static MemoryManager {
    &EXTERNAL_HEAP
}

#[esp_hal::main]
fn main() -> ! {
    let peripherals = esp_hal::init(esp_hal::Config::default());
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

    // Revision v0.0 has no supported PSRAM mapping. The target's Rust flags
    // enable Espressif's PSRAM instruction workaround for later revisions.
    let external = (esp_hal::efuse::chip_revision().major != 0).then(|| {
        let psram = Psram::new(
            peripherals.PSRAM,
            PsramConfig {
                cache_speed: PsramCacheSpeed::PsramCacheF40mS40m,
                ..PsramConfig::default()
            },
        );
        let (start, size) = psram.raw_parts();
        // SAFETY: the data heap is the only owner of this PSRAM mapping.
        let registered = unsafe { external_heap().init_psram_from_hal(&psram) };
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
        display,
        sessions: &[session::CONSOLE],
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
    firmware::psram_self_test::run(external_heap(), report);
}

#[panic_handler]
fn panic(information: &PanicInfo<'_>) -> ! {
    esp_println::println!("Xila panic: {information}");
    loop {
        core::hint::spin_loop();
    }
}
