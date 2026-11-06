//! SPI bus controller over esp-hal's master driver.
//!
//! [`EspSpi`] owns the bus and one software-driven chip-select output per
//! endpoint. A reservation takes the whole bus out of a critical-section mutex
//! and returns it on drop, so transactions cannot overlap without unsafe code.
//! Each transaction applies the device settings, and each segment its own
//! frequency override, through esp-hal's `apply_config`.
//!
//! Limits of this controller:
//! - word size is 8 bits only;
//! - `DelayNs` segments are unsupported, as in the contract's reference mock;
//! - the contract has no I/O error variant yet, so driver errors are reported
//!   as `SpiError::Unsupported`. Adding one belongs in `peripherals::spi`.

use core::cell::RefCell;

use critical_section::Mutex;
use embedded_hal::spi::{Operation, SpiBus};
use esp_hal::{
    Blocking,
    gpio::{Level, Output, OutputConfig},
    spi::{
        BitOrder as HalBitOrder, Mode as HalMode,
        master::{Config, Spi as HalSpi},
    },
    time::Rate,
};
use peripherals::spi::{
    BitOrder, ChipSelect, DeviceConfig, Mode, Segment, SpiController, SpiError, SpiReservation,
};

/// Highest SPI clock the platform allows. Conservative until validated on boards.
pub const MAX_FREQUENCY_HZ: u32 = 40_000_000;

/// Largest transfer handled in one driver call, so the controller never needs
/// a heap buffer.
const CHUNK: usize = 64;

struct Inner<const N: usize> {
    bus: HalSpi<'static, Blocking>,
    chip_selects: [Output<'static>; N],
}

/// Shared SPI bus with `N` software-controlled chip-select endpoints.
pub struct EspSpi<const N: usize> {
    inner: Mutex<RefCell<Option<Inner<N>>>>,
}

impl<const N: usize> EspSpi<N> {
    /// Takes ownership of the bus and the chip-select outputs. Every chip select
    /// is driven high (deasserted) before the controller is returned.
    pub fn new(bus: HalSpi<'static, Blocking>, chip_selects: [Output<'static>; N]) -> Self {
        let mut chip_selects = chip_selects;
        for chip_select in &mut chip_selects {
            chip_select.set_high();
        }
        Self {
            inner: Mutex::new(RefCell::new(Some(Inner { bus, chip_selects }))),
        }
    }

    /// Creates an output driven high. Use it to build the chip-select array
    /// from owned pins during chip initialisation.
    pub fn deasserted(pin: impl esp_hal::gpio::OutputPin + 'static) -> Output<'static> {
        Output::new(pin, Level::High, OutputConfig::default())
    }
}

impl<const N: usize> SpiController for EspSpi<N> {
    type Reservation<'a>
        = EspSpiReservation<'a, N>
    where
        Self: 'a;

    fn reserve(&self) -> Result<EspSpiReservation<'_, N>, SpiError> {
        let taken = critical_section::with(|cs| self.inner.borrow(cs).borrow_mut().take())
            .ok_or(SpiError::Busy)?;

        Ok(EspSpiReservation {
            controller: self,
            inner: Some(taken),
        })
    }
}

/// Exclusive use of the bus. Dropping it returns the driver to its controller.
pub struct EspSpiReservation<'a, const N: usize> {
    controller: &'a EspSpi<N>,
    inner: Option<Inner<N>>,
}

impl<const N: usize> Drop for EspSpiReservation<'_, N> {
    fn drop(&mut self) {
        if let Some(inner) = self.inner.take() {
            critical_section::with(|cs| {
                *self.controller.inner.borrow(cs).borrow_mut() = Some(inner);
            });
        }
    }
}

impl<const N: usize> SpiReservation for EspSpiReservation<'_, N> {
    fn transaction<'s, I>(
        &mut self,
        chip_select: ChipSelect,
        config: DeviceConfig,
        segments: I,
    ) -> Result<(), SpiError>
    where
        I: IntoIterator<Item = Segment<'s>>,
    {
        let index = usize::from(chip_select.index());
        if index >= N {
            return Err(SpiError::InvalidChipSelect);
        }
        let inner = self.inner.as_mut().ok_or(SpiError::Busy)?;

        let mut asserted = false;
        let result = run_segments(inner, index, config, &mut asserted, segments);
        if asserted {
            inner.chip_selects[index].set_high();
        }
        result
    }
}

fn run_segments<'s, const N: usize>(
    inner: &mut Inner<N>,
    index: usize,
    config: DeviceConfig,
    asserted: &mut bool,
    segments: impl IntoIterator<Item = Segment<'s>>,
) -> Result<(), SpiError> {
    for segment in segments {
        let frequency_hz = segment.frequency_hz.unwrap_or(config.frequency_hz);
        let bits_per_word = segment.bits_per_word.unwrap_or(config.bits_per_word);
        validate_frequency(frequency_hz)?;
        if bits_per_word != 8 {
            return Err(SpiError::BitsPerWordNotSupported);
        }

        match &segment.operation {
            Operation::Transfer(read, write) if read.len() != write.len() => {
                return Err(SpiError::InvalidSegment);
            }
            Operation::DelayNs(_) => return Err(SpiError::Unsupported),
            _ => {}
        }

        apply_device_config(inner, frequency_hz, config)?;

        if !*asserted {
            inner.chip_selects[index].set_low();
            *asserted = true;
        }
        transfer_operation(&mut inner.bus, segment.operation)?;

        if segment.cs_change {
            inner.chip_selects[index].set_high();
            *asserted = false;
        }
    }

    Ok(())
}

fn validate_frequency(frequency_hz: u32) -> Result<(), SpiError> {
    if frequency_hz == 0 || frequency_hz > MAX_FREQUENCY_HZ {
        Err(SpiError::FrequencyNotSupported)
    } else {
        Ok(())
    }
}

fn apply_device_config<const N: usize>(
    inner: &mut Inner<N>,
    frequency_hz: u32,
    config: DeviceConfig,
) -> Result<(), SpiError> {
    let order = match config.bit_order {
        BitOrder::MsbFirst => HalBitOrder::MsbFirst,
        BitOrder::LsbFirst => HalBitOrder::LsbFirst,
    };
    let mode = match config.mode {
        Mode::Mode0 => HalMode::_0,
        Mode::Mode1 => HalMode::_1,
        Mode::Mode2 => HalMode::_2,
        Mode::Mode3 => HalMode::_3,
    };
    let hal_config = Config::default()
        .with_frequency(Rate::from_hz(frequency_hz))
        .with_mode(mode)
        .with_read_bit_order(order)
        .with_write_bit_order(order);

    inner
        .bus
        .apply_config(&hal_config)
        .map_err(|_| SpiError::FrequencyNotSupported)
}

/// Runs one operation in bounded chunks. The contract's `Read`/`Write`
/// segments map onto esp-hal's `SpiBus` methods directly.
fn transfer_operation(
    bus: &mut HalSpi<'static, Blocking>,
    operation: Operation<'_, u8>,
) -> Result<(), SpiError> {
    let mut scratch = [0u8; CHUNK];
    let result = match operation {
        Operation::Read(buffer) => buffer.chunks_mut(CHUNK).try_for_each(|chunk| {
            let length = chunk.len();
            scratch[..length].fill(0);
            SpiBus::transfer(bus, chunk, &scratch[..length])
        }),
        Operation::Write(buffer) => buffer
            .chunks(CHUNK)
            .try_for_each(|chunk| SpiBus::write(bus, chunk)),
        Operation::Transfer(read, write) => read
            .chunks_mut(CHUNK)
            .zip(write.chunks(CHUNK))
            .try_for_each(|(read, write)| SpiBus::transfer(bus, read, write)),
        Operation::TransferInPlace(buffer) => buffer
            .chunks_mut(CHUNK)
            .try_for_each(|chunk| SpiBus::transfer_in_place(bus, chunk)),
        // Rejected by `run_segments` before any chip-select activity.
        Operation::DelayNs(_) => Ok(()),
    };

    result.map_err(|_| SpiError::Unsupported)
}
