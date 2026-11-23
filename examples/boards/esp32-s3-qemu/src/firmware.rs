//! Espressif QEMU's ESP32-S3 machine: 4 MB of flash, the console on UART0 and
//! the emulator's virtual RGB panel, which renders one test frame at boot.

extern crate alloc;

use core::panic::PanicInfo;

use drivers_espressif::random::RandomDevice;
use esp_hal::{
    timer::systimer::SystemTimer,
    uart::{Config as UartConfiguration, Uart},
};
use esp_storage::FlashStorage;
use firmware::{Board, MemoryReport, StoragePartition, parse_decimal, session};

mod display;

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
        .with_tx(peripherals.GPIO43)
        .with_rx(peripherals.GPIO44);

    firmware::run(Board {
        name: "ESP32-S3 (Espressif QEMU)",
        console: serial.into(),
        flash_storage: FlashStorage::new(peripherals.FLASH),
        storage: STORAGE,
        random_device: RandomDevice::new(peripherals.RNG, peripherals.ADC1),
        has_entropy_source: true,
        // The smoke test checks that the device returns changing values.
        check_random_device: true,
        memory: MemoryReport {
            internal_sram_size: 512 * 1024,
            external: None,
        },
        memory_check: None,
        system_check: Some(display::render),
        display: None,
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
