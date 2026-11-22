//! Wireless-Tag WT32-SC01 with the ESP32-WROVER-B module.

use core::{mem::MaybeUninit, panic::PanicInfo};

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
const INTERNAL_HEAP_SIZE: usize = 160 * 1024;
static INTERNAL_HEAP: StaticCell<[MaybeUninit<u8>; INTERNAL_HEAP_SIZE]> = StaticCell::new();

// Keep external storage explicit. ESP32 hardware atomics and flash ROM calls
// cannot use PSRAM buffers. The global allocator has no way to distinguish
// those allocations from ordinary data.
static EXTERNAL_HEAP: MemoryManager = MemoryManager::new();

#[esp_hal::main]
fn main() -> ! {
    let peripherals = esp_hal::init(esp_hal::Config::default());
    let reservation = INTERNAL_HEAP.init_with(|| [MaybeUninit::uninit(); INTERNAL_HEAP_SIZE]);
    // SAFETY: the reservation is exclusive and linked into internal data SRAM.
    unsafe { firmware::initialize_heap_from_slice(reservation) };

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
        let registered = unsafe { EXTERNAL_HEAP.init_psram_from_hal(&psram) };
        ExternalMemory {
            start: start as usize,
            size,
            registered,
        }
    });

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
        system_check: None,
        display: None,
        sessions: &[session::CONSOLE],
    })
}

fn check_external_memory(report: &MemoryReport) {
    if let Some(external) = report.external {
        if external.registered {
            firmware::psram_self_test::check(
                &EXTERNAL_HEAP,
                external.start..external.start + external.size,
            );
        }
    } else {
        xila::log::warning!("Xila: PSRAM unavailable on ESP32 revision v0.0");
    }
}

#[panic_handler]
fn panic(information: &PanicInfo<'_>) -> ! {
    esp_println::println!("Xila panic: {information}");
    loop {
        core::hint::spin_loop();
    }
}
