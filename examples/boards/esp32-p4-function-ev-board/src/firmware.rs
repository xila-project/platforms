//! Espressif's ESP32-P4X-Function-EV-Board: chip revision v3.0 or later,
//! 16 MB of flash, 32 MB of PSRAM and the console on UART0.
//!
//! esp-hal 1.2.2 cannot enable the ESP32-P4 entropy source
//! (<https://github.com/esp-rs/esp-hal/issues/5982>), so the board has none.

use core::panic::PanicInfo;

use drivers_espressif::random::RandomDevice;
use esp_hal::{
    psram::{Psram, PsramConfig},
    timer::systimer::SystemTimer,
    uart::{Config as UartConfiguration, Uart},
};
use esp_storage::FlashStorage;
use firmware::{Board, ExternalMemory, MemoryReport, StoragePartition, parse_decimal, session};

esp_bootloader_esp_idf::esp_app_desc!();

/// The `storage` partition of `partitions.csv`.
const STORAGE: StoragePartition = StoragePartition {
    offset: parse_decimal(env!("XILA_STORAGE_OFFSET")) as u32,
    length: parse_decimal(env!("XILA_STORAGE_LENGTH")) as u32,
};

#[esp_hal::main]
fn main() -> ! {
    let peripherals = esp_hal::init(esp_hal::Config::default());
    let heap = firmware::initialize_heap();

    let psram = Psram::new(peripherals.PSRAM, PsramConfig::default());
    let (start, size) = psram.raw_parts();
    // SAFETY: The detected PSRAM is reserved exclusively for this allocator.
    let registered = unsafe { heap.init_psram_from_hal(&psram) };

    let system_timer = SystemTimer::new(peripherals.SYSTIMER);
    drivers_espressif::time::initialize(system_timer.alarm0);

    let serial = Uart::new(peripherals.UART0, UartConfiguration::default())
        .expect("Failed to configure UART0")
        .with_tx(peripherals.GPIO37)
        .with_rx(peripherals.GPIO38);

    firmware::run(Board {
        name: "ESP32-P4X-Function-EV-Board",
        console: serial.into(),
        flash_storage: FlashStorage::new(peripherals.FLASH),
        storage: STORAGE,
        random_device: RandomDevice::from_entropy_source(|_| {
            Err(xila::file_system::IoErrorKind::DeviceUnavailable)
        }),
        has_entropy_source: false,
        check_random_device: false,
        memory: MemoryReport {
            internal_sram_size: 768 * 1024,
            external: Some(ExternalMemory {
                start: start as usize,
                size,
                registered,
            }),
        },
        memory_check: None,
        system_check: None,
        display: None,
        sd_card: None,
        gpio: None,
        sessions: &[session::CONSOLE],
    })
}

#[panic_handler]
fn panic(information: &PanicInfo<'_>) -> ! {
    esp_println::println!("Xila panic: {information}");
    loop {
        core::hint::spin_loop();
    }
}
