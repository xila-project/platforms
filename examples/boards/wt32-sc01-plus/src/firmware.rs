//! Wireless-Tag WT32-SC01 Plus: an ESP32-S3 on the WT32-S3-WROVER-N16R2
//! module, with 16 MB of flash and 2 MB of quad SPI PSRAM.
//!
//! - Console: the USB-C port is wired to the chip's USB pins (GPIO 19 and 20),
//!   with no USB to UART bridge. By default the console, the logger and panic
//!   messages use the USB Serial/JTAG controller (303a:1001). With the
//!   `usb-human-interface` feature, the chip's single USB PHY is given to the USB
//!   OTG controller instead, and the console runs over the CDC-ACM interface of a
//!   composite USB device that also carries the human interface emulation; see
//!   the board README.
//! - Clocks: the CPU runs at 240 MHz, and the flash, in quad I/O mode, and
//!   the PSRAM at 80 MHz. Enable the `boot-benchmark` feature to log their
//!   speed once at boot.
//! - PSRAM: added to the heap, and checked at boot by [`firmware::psram_self_test`].
//! - Flash: 16 MB, partitioned by `partitions.csv`.
//! - microSD card: a slot that carries the card's clock on GPIO 39, command
//!   on GPIO 40, data 0 on GPIO 38 and data 3 (chip select) on GPIO 41. The
//!   firmware reads the card in native SD mode with a 1-bit bus, at boot only:
//!   the slot has no card-detect line. GPIO 38 also carries the RS-485
//!   connector's receive line (see the board's README), which this firmware
//!   does not use.
//! - Screen: a 480 × 320 ST7796 panel on the 8080 bus, its dimmable backlight and an
//!   FT6336U touch panel, in [`screen`]. The graphical shell runs on it, and
//!   the command-line shell on the console, as two sessions.
//! - GPIO: the six pins of the Extended IO connector, mounted at
//!   `/devices/gpio/pin<number>`, in [`gpio`].
//! - WebAssembly runtime: mounted at `/applications/executables/wasm` with lazy
//!   function translation by default; large Wasmi buffers (4096 bytes and up,
//!   including guest linear memory and module source) land in PSRAM.

use alloc::{boxed::Box, format};
use core::{fmt::Write, panic::PanicInfo};

use drivers_espressif::{random::RandomDevice, sd_card::SdCard};
use drivers_shared::devices::SectorBlockDevice;
use esp_hal::{
    clock::CpuClock,
    peripherals::{GPIO38, GPIO39, GPIO40, GPIO41, SDHOST, USB_DEVICE},
    psram::{FlashFreq, Psram, PsramConfig, PsramMode, SpiRamFreq},
    timer::systimer::SystemTimer,
    usb::usb_serial_jtag::UsbSerialJtag,
};
use esp_storage::FlashStorage;
use firmware::{
    Board, ExternalMemory, MemoryReport, OpenedSdCard, SdCardSlot, StoragePartition, parse_decimal,
    session,
};

#[cfg(feature = "boot-benchmark")]
mod boot_benchmark;
mod gpio;
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
    #[cfg(feature = "bluetooth")]
    {
        use core::{mem::MaybeUninit, ptr::addr_of_mut};

        // The radio adds internal code and data. Move 24 KiB of heap out of
        // the main stack bank into SRAM reclaimed after the bootloader exits.
        #[unsafe(link_section = ".dram2_uninit")]
        static mut RECLAIMED_HEAP: [MaybeUninit<u8>; 24 * 1024] =
            [MaybeUninit::uninit(); 24 * 1024];
        // SAFETY: The linker reserves this SRAM slice for the heap. It is
        // DMA-capable, accessible with the cache suspended and initialized once.
        assert!(unsafe { heap.init_reclaimed_internal(&mut *addr_of_mut!(RECLAIMED_HEAP)) });
    }

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

    // The console: the USB Serial/JTAG controller by default, or, with the
    // `usb-human-interface` feature, the CDC-ACM interface of the composite USB
    // device on the USB OTG controller. The ESP32-S3 has one USB PHY, shared by
    // both controllers, so only one of them can use the port.
    #[cfg(not(feature = "usb-human-interface"))]
    let console = UsbSerialJtag::new(peripherals.USB_DEVICE).into();
    #[cfg(feature = "usb-human-interface")]
    let usb_human_interface = drivers_espressif::human_interface::UsbHumanInterface::new(
        peripherals.USB_FS,
        peripherals.GPIO20,
        peripherals.GPIO19,
        drivers_espressif::human_interface::UsbHumanInterfaceConfig::default(),
    )
    .expect("The USB human interface device was already initialized");
    #[cfg(feature = "usb-human-interface")]
    let console = usb_human_interface.console;

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
        ledc: peripherals.LEDC,
        i2c: peripherals.I2C0,
        touch_data: peripherals.GPIO6,
        touch_clock: peripherals.GPIO5,
    });

    let real_time_clock = Box::leak(Box::new(
        drivers_espressif::real_time_clock::InternalRealTimeClock::new(
            esp_hal::rtc_cntl::Rtc::new(peripherals.RTC_TIMER),
            i64::try_from(firmware::BUILD_EPOCH)
                .expect("the build date fits in a signed timestamp"),
        ),
    ));
    let board = Board {
        name: "WT32-SC01 Plus",
        console,
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
        sd_card: Some(sd_card_slot(
            peripherals.SDHOST,
            peripherals.GPIO39,
            peripherals.GPIO40,
            peripherals.GPIO38,
            peripherals.GPIO41,
        )),
        gpio: Some(gpio::controller()),
        #[cfg(feature = "usb-human-interface")]
        human_interface: Some(usb_human_interface.controller),
        real_time_clock: Some(real_time_clock),
        sessions: if display.is_some() {
            &[session::CONSOLE, session::GRAPHICAL]
        } else {
            &[session::CONSOLE]
        },
        display,
    };
    firmware::run_with_wifi(
        board,
        drivers_espressif::wifi::Hardware::Station(drivers_espressif::wifi::Peripherals {
            radio: peripherals.WIFI,
            #[cfg(feature = "bluetooth")]
            bluetooth: peripherals.BT,
            scheduler_timer: peripherals.TIMG0,
            scheduler_interrupt: peripherals.FROM_CPU_INTR0,
        }),
    )
}

/// The microSD slot. The firmware opens the card at boot, once.
///
/// The slot's lines go through the GPIO matrix to the SDHOST controller, in
/// native SD mode with a 1-bit bus. Data line 3 (`chip_select`) is an output
/// that stays high, so that the card selects SD mode and not SPI mode.
fn sd_card_slot(
    host: SDHOST<'static>,
    clock: GPIO39<'static>,
    command: GPIO40<'static>,
    data0: GPIO38<'static>,
    chip_select: GPIO41<'static>,
) -> SdCardSlot {
    SdCardSlot::new(move || {
        let card = SdCard::open_native(host, clock, command, data0, chip_select)
            .map_err(|error| format!("native SD mode, 1-bit bus: {error}"))?;
        let description = card.describe();
        let device: &'static SectorBlockDevice<_> =
            Box::leak(Box::new(SectorBlockDevice::new(card)));
        Ok(OpenedSdCard::new(description, device))
    })
}

/// The board's memory check: the PSRAM self-test, followed by the boot
/// benchmark when the `boot-benchmark` feature is enabled.
fn check_memory(report: &MemoryReport) {
    firmware::psram_self_test::run(firmware::heap(), report);
    #[cfg(feature = "boot-benchmark")]
    boot_benchmark::run(report);
}

/// Writes the panic message to the console. esp-println writes to UART0,
/// which this board does not expose. With the `usb-human-interface` feature the
/// USB OTG controller owns the chip's single USB PHY, so this write through the
/// USB Serial/JTAG controller reaches no host.
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
