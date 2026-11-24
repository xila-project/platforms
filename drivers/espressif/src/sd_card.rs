//! A microSD card in native SD mode, on the `SDHOST` controller of the ESP32-S3.
//!
//! [`SdCard`] runs the card protocol of the `sdio` crate over esp-hal's
//! [`esp_hal::sdmmc`] driver and implements [`SectorStorage`]. Wrap it in a
//! [`drivers_shared::devices::SectorBlockDevice`] to get a Core block device.
//!
//! # Bus
//!
//! The controller routes its signals through the GPIO matrix, so any GPIO can
//! carry them. [`SdCard::open_native`] uses a 1-bit bus: the clock, the
//! command line and data line 0.
//!
//! The chip select line (data line 3) is an output that stays high from
//! before the first command. A card that sees it low at the first `CMD0`
//! selects SPI mode, which the controller does not support.
//!
//! # Blocking calls
//!
//! The `sdio` crate is asynchronous, and `Slot` in async mode waits for an
//! interrupt of the controller. [`SdCard`] drives each operation with
//! [`embassy_futures::block_on`]: the processor spins until the transfer is
//! over, and interrupts stay enabled. This is the same contract as the flash
//! partition of the firmware, which Core's LittleFS needs, because it calls
//! storage from C callbacks that cannot suspend.
//!
//! # Memory
//!
//! The controller's DMA reads and writes one 512-byte sector buffer. The
//! buffer is a `static` in internal memory, so the transfer needs no bounce
//! copy. Nothing else is allocated per transfer.

use alloc::{boxed::Box, format, string::String};
use core::fmt;

use aligned::{A4, Aligned};
use block_device_driver::BlockDevice as AsynchronousBlockDevice;
use drivers_shared::devices::{SECTOR_SIZE, SectorStorage};
use embassy_futures::block_on;
use embedded_hal_async::delay::DelayNs;
use esp_hal::{
    delay::Delay,
    gpio::{Level, Output, OutputConfig, OutputPin},
    peripherals::SDHOST,
    sdmmc::{Config, ConfigError, SdHostController, Slot, SlotClk, SlotCmd, SlotConfig, SlotData},
};
use file_system::{IoErrorKind, Result};
use sdio::{DefaultBlockDevice, MmcBus, MmcError, sd::Card};
use static_cell::StaticCell;

/// The card clock that [`SdCard::open_native`] asks for after the
/// identification phase. The default speed of an SD card is 25 MHz. 20 MHz is
/// ESP-IDF's default and leaves margin for the GPIO matrix and the traces.
pub const CARD_FREQUENCY: u32 = 20_000_000;

/// The sector buffer of the DMA, in internal memory.
static SECTOR_BUFFER: StaticCell<Aligned<A4, [u8; SECTOR_SIZE]>> = StaticCell::new();

/// Why a card could not be opened.
#[derive(Debug)]
pub enum SdCardError {
    /// The host controller refused its configuration.
    Controller(ConfigError),
    /// The card did not complete its initialization, or no card answered.
    Card(MmcError),
    /// The card holds more than 2^32 sectors, or none.
    Capacity,
    /// A card was already opened: the buffer of the DMA exists once.
    AlreadyOpen,
}

impl fmt::Display for SdCardError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Controller(error) => write!(formatter, "host controller: {error}"),
            Self::Card(error) => write!(formatter, "{error}"),
            Self::Capacity => formatter.write_str("unsupported capacity"),
            Self::AlreadyOpen => formatter.write_str("already opened"),
        }
    }
}

/// An asynchronous delay that waits in a loop. The `sdio` crate needs a few
/// delays between commands, and the callers block anyway.
pub struct BusyDelay(Delay);

impl BusyDelay {
    pub fn new() -> Self {
        Self(Delay::new())
    }
}

impl Default for BusyDelay {
    fn default() -> Self {
        Self::new()
    }
}

impl DelayNs for BusyDelay {
    async fn delay_ns(&mut self, nanoseconds: u32) {
        self.0.delay_nanos(nanoseconds);
    }
}

/// An initialized SD card.
pub struct SdCard<B: MmcBus> {
    device: DefaultBlockDevice<Card, B, BusyDelay>,
    buffer: &'static mut Aligned<A4, [u8; SECTOR_SIZE]>,
    sectors: u32,
    bus_description: &'static str,
    /// The chip select line of the slot, held high for SD mode.
    _chip_select: Option<Output<'static>>,
}

impl SdCard<Slot<'static, 0, esp_hal::Async>> {
    /// Opens the card in native SD mode with a 1-bit bus.
    ///
    /// `chip_select` is the line that carries data line 3 of the card. It
    /// goes high before the first command. The firmware calls this function
    /// once, at boot: it blocks until the card answers or the initialization
    /// fails, which takes a few milliseconds with a card and at most a few
    /// hundred without.
    pub fn open_native(
        host: SDHOST<'static>,
        clock: impl SlotClk<'static, 0>,
        command: impl SlotCmd<'static, 0>,
        data0: impl SlotData<'static, 0, 0>,
        chip_select: impl OutputPin + 'static,
    ) -> core::result::Result<Self, SdCardError> {
        // Data line 3 must be high when the card receives `CMD0`.
        let chip_select = Output::new(chip_select, Level::High, OutputConfig::default());

        let controller =
            SdHostController::new(host, Config::default()).map_err(SdCardError::Controller)?;
        // The slot borrows the controller: it lives as long as the firmware.
        let controller: &'static SdHostController<'static> = Box::leak(Box::new(controller));
        let slot = controller
            .slot::<0>(SlotConfig::default())
            .map_err(SdCardError::Controller)?
            .with_clk(clock)
            .with_cmd(command)
            .with_data0(data0)
            .into_async();

        Self::open(slot, "native SD mode, 1-bit bus", Some(chip_select))
    }
}

impl<B: MmcBus> SdCard<B> {
    fn open(
        bus: B,
        bus_description: &'static str,
        chip_select: Option<Output<'static>>,
    ) -> core::result::Result<Self, SdCardError> {
        let buffer = SECTOR_BUFFER
            .try_init(Aligned([0; SECTOR_SIZE]))
            .ok_or(SdCardError::AlreadyOpen)?;
        let mut device = block_on(DefaultBlockDevice::new_sd_card(
            bus,
            CARD_FREQUENCY,
            BusyDelay::new(),
        ))
        .map_err(SdCardError::Card)?;
        let bytes = block_on(device.size()).map_err(SdCardError::Card)?;
        let sectors =
            u32::try_from(bytes / SECTOR_SIZE as u64).map_err(|_| SdCardError::Capacity)?;
        if sectors == 0 {
            return Err(SdCardError::Capacity);
        }
        Ok(Self {
            device,
            buffer,
            sectors,
            bus_description,
            _chip_select: chip_select,
        })
    }

    /// The capacity in bytes.
    pub fn capacity(&self) -> u64 {
        u64::from(self.sectors) * SECTOR_SIZE as u64
    }

    /// A one-line description for the boot log: the card type, the product,
    /// the capacity, the bus mode and the card clock.
    pub fn describe(&self) -> String {
        let card = self.device.card();
        let capacity = self.capacity();
        // SDSC, SDHC and SDXC differ by capacity: up to 2 GiB, up to 32 GiB,
        // and more.
        let kind = if !card.ocr.high_capacity() {
            "SDSC"
        } else if capacity <= 32 * 1024 * 1024 * 1024 {
            "SDHC"
        } else {
            "SDXC"
        };
        let (month, year) = card.cid.manufacturing_date();
        let mebibytes = capacity / (1024 * 1024);
        format!(
            "{kind} \"{}\" (manufacturer {:#04x}, revision {}, {year}-{month:02}), \
             {}.{:02} GiB ({} sectors of {SECTOR_SIZE} bytes), {}, {} kHz",
            card.cid.product_name(),
            card.cid.manufacturer_id(),
            card.cid.product_revision(),
            mebibytes / 1024,
            mebibytes % 1024 * 100 / 1024,
            self.sectors,
            self.bus_description,
            self.device.freq() / 1000,
        )
    }
}

fn map_error(error: MmcError) -> IoErrorKind {
    match error {
        MmcError::Timeout | MmcError::Busy => IoErrorKind::TimedOut,
        MmcError::BlockSize | MmcError::BusWidth | MmcError::Unsupported => {
            IoErrorKind::Unsupported
        }
        _ => IoErrorKind::InputOutput,
    }
}

// SAFETY: the bus handle of esp-hal (`Slot`) is a marker for state that the
// driver keeps in statics behind critical sections: the controller registers,
// the transfer state and the interrupt waker. Moving the handle to another
// thread changes none of it. `SectorBlockDevice` allows one transfer at a
// time.
unsafe impl<B: MmcBus> Send for SdCard<B> {}

impl<B: MmcBus> SectorStorage for SdCard<B> {
    fn sector_count(&self) -> u32 {
        self.sectors
    }

    fn read_sector(&mut self, index: u32, sector: &mut [u8; SECTOR_SIZE]) -> Result<()> {
        block_on(
            self.device
                .read(index, core::slice::from_mut(&mut *self.buffer)),
        )
        .map_err(map_error)?;
        sector.copy_from_slice(&self.buffer[..]);
        Ok(())
    }

    fn write_sector(&mut self, index: u32, sector: &[u8; SECTOR_SIZE]) -> Result<()> {
        self.buffer.copy_from_slice(sector);
        block_on(
            self.device
                .write(index, core::slice::from_ref(&*self.buffer)),
        )
        .map_err(map_error)
    }
}
