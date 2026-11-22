//! Wireless-Tag WT32-SC01 Plus: an ESP32-S3 on the WT32-S3-WROVER-N16R2
//! module, with 16 MB of flash and 2 MB of quad SPI PSRAM.
//!
//! - Console: the USB-C port is wired to the chip's USB Serial/JTAG controller
//!   (303a:1001), with no USB to UART bridge, so the console, the logger and
//!   panic messages use that controller.
//! - Clocks: the CPU runs at 240 MHz, and the flash, in quad I/O mode, and
//!   the PSRAM at 80 MHz. Enable the `boot-benchmark` feature to log their
//!   speed once at boot.
//! - PSRAM: added to the heap, and checked at boot by [`firmware::psram_self_test`].
//! - Flash: 16 MB, partitioned by `partitions.csv`.
//! - Screen: a 480 × 320 ST7796 panel on the 8080 bus, its backlight and an
//!   FT6336U touch panel, in [`screen`]. The graphical shell runs on it, and
//!   the command-line shell on the console, as two sessions.
//! - WebAssembly runtime: mounted at `/applications/executables/wasm` with lazy
//!   function translation by default; large Wasmi buffers (4096 bytes and up,
//!   including guest linear memory and module source) land in PSRAM.

use core::{fmt::Write, panic::PanicInfo};

use drivers_espressif::random::RandomDevice;
use esp_hal::{
    clock::CpuClock,
    peripherals::USB_DEVICE,
    psram::{FlashFreq, Psram, PsramConfig, PsramMode, SpiRamFreq},
    timer::systimer::SystemTimer,
    usb::usb_serial_jtag::UsbSerialJtag,
};
use esp_storage::FlashStorage;
use firmware::{Board, ExternalMemory, MemoryReport, StoragePartition, parse_decimal, session};

#[cfg(feature = "boot-benchmark")]
mod boot_benchmark;
mod screen;

esp_bootloader_esp_idf::esp_app_desc!();

/// The `storage` partition of `partitions.csv`.
const STORAGE: StoragePartition = StoragePartition {
    offset: parse_decimal(env!("XILA_STORAGE_OFFSET")) as u32,
    length: parse_decimal(env!("XILA_STORAGE_LENGTH")) as u32,
};

/// The CPU clock. esp-hal's default for the ESP32-S3 is 80 MHz; 240 MHz is the
/// chip's maximum and ESP-IDF's usual setting. The peripherals this board uses
/// take their clocks from the 80 MHz APB clock, the crystal or the PLL, not
/// from the CPU clock, so their rates do not change.
const CPU_CLOCK: CpuClock = CpuClock::_240MHz;

/// The PSRAM clock. esp-hal's default is 40 MHz. ESP-IDF supports quad PSRAM
/// at 80 MHz with quad flash at 80 MHz on the ESP32-S3: both share one clock,
/// and only 120 MHz needs the phase calibration that esp-hal does not do.
/// esp-hal then divides an 80 MHz memory SPI core clock by one for both.
const PSRAM_FREQUENCY: SpiRamFreq = SpiRamFreq::Freq80m;

/// The flash clock. The PSRAM setup reprograms the flash clock divider too, so
/// this, not the image header, sets the flash clock once the firmware runs.
const FLASH_FREQUENCY: FlashFreq = FlashFreq::FlashFreq80m;

#[esp_hal::main]
fn main() -> ! {
    let peripherals = esp_hal::init(esp_hal::Config::default().with_cpu_clock(CPU_CLOCK));
    let heap = firmware::initialize_heap();

    // Quad SPI PSRAM. Probing for octal PSRAM first, as the automatic mode
    // does, would drive pins the module does not use for it.
    let psram = Psram::new(
        peripherals.PSRAM,
        PsramConfig {
            mode: PsramMode::QuadSpi,
            ram_frequency: PSRAM_FREQUENCY,
            flash_frequency: FLASH_FREQUENCY,
            ..PsramConfig::default()
        },
    );
    let (start, size) = psram.raw_parts();
    // SAFETY: The detected PSRAM is reserved exclusively for this allocator.
    let registered = unsafe { heap.init_psram_from_hal(&psram) };

    let system_timer = SystemTimer::new(peripherals.SYSTIMER);
    drivers_espressif::time::initialize(system_timer.alarm0);

    let console = UsbSerialJtag::new(peripherals.USB_DEVICE);

    let display = screen::start(screen::Pins {
        lcd_cam: peripherals.LCD_CAM,
        dma: peripherals.DMA_CH0,
        data: (
            peripherals.GPIO9,
            peripherals.GPIO46,
            peripherals.GPIO3,
            peripherals.GPIO8,
            peripherals.GPIO18,
            peripherals.GPIO17,
            peripherals.GPIO16,
            peripherals.GPIO15,
        ),
        write_clock: peripherals.GPIO47,
        command_data: peripherals.GPIO0,
        reset: peripherals.GPIO4,
        backlight: peripherals.GPIO45,
        i2c: peripherals.I2C0,
        touch_data: peripherals.GPIO6,
        touch_clock: peripherals.GPIO5,
    });

    let board = Board {
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
        memory_check: Some(check_memory),
        system_check: Some(screen::log_report),
        sessions: if display.is_some() {
            &[session::CONSOLE, session::GRAPHICAL_TOUCH]
        } else {
            &[session::CONSOLE]
        },
        display,
    };
    firmware::run_with_wifi(
        board,
        drivers_espressif::wifi::Hardware::Station(drivers_espressif::wifi::Peripherals {
            radio: peripherals.WIFI,
            scheduler_timer: peripherals.TIMG0,
            scheduler_interrupt: peripherals.FROM_CPU_INTR0,
        }),
    )
}

/// The board's memory check: the PSRAM self-test, followed by the boot
/// benchmark when the `boot-benchmark` feature is enabled.
fn check_memory(report: &MemoryReport) {
    firmware::psram_self_test::run(report);
    #[cfg(feature = "boot-benchmark")]
    boot_benchmark::run(report);
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
