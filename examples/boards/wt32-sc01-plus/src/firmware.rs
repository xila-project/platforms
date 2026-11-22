//! Wireless-Tag WT32-SC01 Plus: an ESP32-S3 on the WT32-S3-WROVER-N16R2
//! module, with 16 MB of flash and 2 MB of quad SPI PSRAM.
//!
//! - Console: the USB-C port is wired to the chip's USB Serial/JTAG controller
//!   (303a:1001), with no USB to UART bridge, so the console, the logger and
//!   panic messages use that controller.
//! - PSRAM: added to the heap, and checked at boot by [`psram_self_test`].
//! - Flash: 16 MB, partitioned by `partitions.csv`.
//! - Display, backlight and touch: not driven yet.

use core::{fmt::Write, panic::PanicInfo};

use drivers_espressif::random::RandomDevice;
use esp_hal::{
    peripherals::USB_DEVICE,
    psram::{Psram, PsramConfig, PsramMode},
    timer::systimer::SystemTimer,
    usb::usb_serial_jtag::UsbSerialJtag,
};
use esp_storage::FlashStorage;
use firmware::{Board, ExternalMemory, MemoryReport, StoragePartition, parse_decimal, session};

mod psram_self_test;

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

    // Quad SPI PSRAM. Probing for octal PSRAM first, as the automatic mode
    // does, would drive pins the module does not use for it.
    let psram = Psram::new(
        peripherals.PSRAM,
        PsramConfig {
            mode: PsramMode::QuadSpi,
            ..PsramConfig::default()
        },
    );
    let (start, size) = psram.raw_parts();
    // SAFETY: The detected PSRAM is reserved exclusively for this allocator.
    let registered = unsafe { heap.init_psram_from_hal(&psram) };

    let system_timer = SystemTimer::new(peripherals.SYSTIMER);
    drivers_espressif::time::initialize(system_timer.alarm0);

    let console = UsbSerialJtag::new(peripherals.USB_DEVICE);

    firmware::run(Board {
        name: "WT32-SC01 Plus",
        console: console.into(),
        flash_storage: FlashStorage::new(peripherals.FLASH),
        storage: STORAGE,
        random_device: RandomDevice::new(peripherals.RNG, peripherals.ADC1),
        has_entropy_source: true,
        check_random_device: true,
        memory: MemoryReport {
            internal_sram_size: 512 * 1024,
            external: Some(ExternalMemory {
                start: start as usize,
                size,
                registered,
            }),
        },
        memory_check: Some(psram_self_test::run),
        system_check: None,
        sessions: &[session::CONSOLE],
    })
}

/// Writes the panic message to the console. esp-println writes to UART0,
/// which this board does not expose.
#[panic_handler]
fn panic(information: &PanicInfo<'_>) -> ! {
    // SAFETY: the firmware has stopped; nothing else uses the controller again.
    let device = unsafe { USB_DEVICE::steal() };
    let mut console = UsbSerialJtag::new(device);
    let _ = write!(console, "\r\nXila panic: {information}\r\n");
    let _ = console.flush_tx();
    loop {
        core::hint::spin_loop();
    }
}
