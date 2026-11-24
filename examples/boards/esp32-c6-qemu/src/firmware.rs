//! Espressif QEMU's ESP32-C6 machine: 4 MB of flash and the console on UART0.
//!
//! The pinned QEMU machine lacks the random register, so the board has no
//! entropy source. It runs the developer-only native executable handler and
//! checks at boot that the CPU can execute from its arena.

use core::panic::PanicInfo;

use drivers_espressif::random::RandomDevice;
use esp_hal::{
    timer::systimer::SystemTimer,
    uart::{Config as UartConfiguration, Uart},
};
use esp_storage::FlashStorage;
use firmware::{Board, MemoryReport, StoragePartition, parse_decimal, session};

esp_bootloader_esp_idf::esp_app_desc!();

/// The `storage` partition of `partitions.csv`.
const STORAGE: StoragePartition = StoragePartition {
    offset: parse_decimal(env!("XILA_STORAGE_OFFSET")) as u32,
    length: parse_decimal(env!("XILA_STORAGE_LENGTH")) as u32,
};

#[esp_hal::main]
fn main() -> ! {
    let peripherals = esp_hal::init(esp_hal::Config::default());
    firmware::initialize_heap();

    let system_timer = SystemTimer::new(peripherals.SYSTIMER);
    drivers_espressif::time::initialize(system_timer.alarm0);

    let serial = Uart::new(peripherals.UART0, UartConfiguration::default())
        .expect("Failed to configure UART0")
        .with_tx(peripherals.GPIO16)
        .with_rx(peripherals.GPIO17);

    firmware::run(Board {
        name: "ESP32-C6 (Espressif QEMU)",
        console: serial.into(),
        flash_storage: FlashStorage::new(peripherals.FLASH),
        storage: STORAGE,
        random_device: RandomDevice::from_entropy_source(|_| {
            Err(xila::file_system::IoErrorKind::DeviceUnavailable)
        }),
        has_entropy_source: false,
        check_random_device: false,
        memory: MemoryReport {
            internal_sram_size: 512 * 1024,
            external: None,
        },
        memory_check: Some(firmware::native::log_self_test),
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
