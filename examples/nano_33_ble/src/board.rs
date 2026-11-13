//! What is specific to the Arduino Nano 33 BLE: taking over from its
//! bootloader, going back to it, and carrying the message of a crash across
//! the reset.

use core::fmt::{self, Write};
use core::mem::MaybeUninit;

use embassy_nrf::pac;

/// `DFU_MAGIC_SERIAL_ONLY_RESET` of ArduinoCore-mbed
/// (`variants/ARDUINO_NANO33BLE/pins_arduino.h`): in `GPREGRET` at a reset, it
/// keeps the bootloader waiting for a firmware.
const BOOTLOADER_MAGIC: u32 = 0xB0;

/// Crashes in a row after which the board stays in its bootloader instead of
/// starting a firmware that cannot stay up.
const CRASH_LIMIT: u32 = 3;

const CRASH_MAGIC: u32 = 0x5843_5248;
const CRASH_MESSAGE_SIZE: usize = 248;

pub enum Reset {
    Application,
    Bootloader,
}

/// Turns off the peripherals the bootloader leaves on: it jumps to the
/// application without resetting them. After a reset this changes nothing.
pub fn quiesce() {
    let ppi = pac::PPI;
    ppi.chenclr().write(|w| w.0 = !0);
    for group in 0..6 {
        ppi.tasks_chg(group).dis().write_value(1);
        ppi.chg(group).write(|w| w.0 = 0);
    }
    // embassy-nrf enables the USB device again from scratch.
    pac::USBD.intenclr().write(|w| w.0 = !0);
    pac::USBD.enable().write(|w| w.0 = 0);
    // CLOCK and POWER are one peripheral, with one set of interrupt enables.
    pac::CLOCK.intenclr().write(|w| w.0 = !0);
    pac::RTC1.tasks_stop().write_value(1);
    pac::RTC1.intenclr().write(|w| w.0 = !0);
    pac::RTC1.evtenclr().write(|w| w.0 = !0);
    pac::UARTE0.intenclr().write(|w| w.0 = !0);
    pac::UARTE0.enable().write(|w| w.0 = 0);
}

/// Stops both clocks and clears their events: `embassy_nrf::init` selects the
/// source of each, starts it and waits for its event. The source of the LFCLK
/// can only be written once the clock has stopped, and a clock that already
/// runs may never give its event again.
pub fn stop_clocks() {
    let clock = pac::CLOCK;
    clock.tasks_lfclkstop().write_value(1);
    clock.tasks_hfclkstop().write_value(1);
    // LFCLKSTAT.STATE (bit 16): the LFCLK runs. HFCLKSTAT.SRC (bit 0): the
    // HFCLK is the crystal. Bounded: a clock that will not stop must not hang
    // the start.
    for _ in 0..1_000_000 {
        if clock.lfclkstat().read().0 & (1 << 16) == 0 && clock.hfclkstat().read().0 & 1 == 0 {
            break;
        }
    }
    clock.events_lfclkstarted().write_value(0);
    clock.events_hfclkstarted().write_value(0);
}

/// Resets the chip, into the firmware or into the bootloader.
pub fn reset(to: Reset) -> ! {
    cortex_m::interrupt::disable();
    let magic = match to {
        Reset::Application => 0,
        Reset::Bootloader => BOOTLOADER_MAGIC,
    };
    pac::POWER.gpregret().write(|w| w.0 = magic);
    cortex_m::peripheral::SCB::sys_reset()
}

/// The hook of the "1200 bps touch" of Arduino.
pub fn enter_bootloader() {
    reset(Reset::Bootloader)
}

/// The identifier of the chip, the serial number of the USB port.
pub fn device_identifier() -> u64 {
    (u64::from(pac::FICR.deviceid(1).read()) << 32) | u64::from(pac::FICR.deviceid(0).read())
}

/// The RAM between the static data and the stack.
pub fn heap_region() -> (*mut u8, usize) {
    unsafe extern "C" {
        static mut __sheap: u8;
        static _heap_end: u8;
    }
    let start = &raw mut __sheap;
    (start, (&raw const _heap_end) as usize - start as usize)
}

/// The part of the internal flash that holds the root file system: its
/// address and its size.
pub fn storage_region() -> (usize, usize) {
    unsafe extern "C" {
        static _storage_start: u8;
        static _storage_size: u8;
    }
    (
        (&raw const _storage_start) as usize,
        (&raw const _storage_size) as usize,
    )
}

#[repr(C)]
struct CrashRecord {
    magic: u32,
    length: u32,
    message: [u8; CRASH_MESSAGE_SIZE],
}

/// Out of reach of the bootloader, which uses the bottom of the RAM at every
/// reset (see `memory.x`).
#[unsafe(link_section = ".crash_record")]
static mut CRASH_RECORD: MaybeUninit<CrashRecord> = MaybeUninit::uninit();

struct Message {
    bytes: [u8; CRASH_MESSAGE_SIZE],
    length: usize,
}

impl Write for Message {
    fn write_str(&mut self, text: &str) -> fmt::Result {
        let count = text.len().min(self.bytes.len() - self.length);
        self.bytes[self.length..self.length + count].copy_from_slice(&text.as_bytes()[..count]);
        self.length += count;
        Ok(())
    }
}

/// Leaves a message for the next start and resets: into the bootloader after
/// [`CRASH_LIMIT`] crashes in a row.
pub fn crash(arguments: fmt::Arguments) -> ! {
    cortex_m::interrupt::disable();
    let mut message = Message {
        bytes: [0; CRASH_MESSAGE_SIZE],
        length: 0,
    };
    let _ = message.write_fmt(arguments);
    // SAFETY: interrupts are off and nothing else runs: the record is ours.
    unsafe {
        (&raw mut CRASH_RECORD)
            .cast::<CrashRecord>()
            .write_volatile(CrashRecord {
                magic: CRASH_MAGIC,
                length: message.length as u32,
                message: message.bytes,
            });
    }
    let count = pac::POWER.gpregret2().read().0 + 1;
    if count >= CRASH_LIMIT {
        pac::POWER.gpregret2().write(|w| w.0 = 0);
        reset(Reset::Bootloader)
    }
    pac::POWER.gpregret2().write(|w| w.0 = count);
    reset(Reset::Application)
}

/// The message the previous run left when it crashed, once.
pub fn take_crash_message(buffer: &mut [u8; CRASH_MESSAGE_SIZE]) -> Option<&str> {
    let record = (&raw mut CRASH_RECORD).cast::<CrashRecord>();
    // SAFETY: the record is plain bytes, valid whatever the RAM holds, and only
    // the start, before any interrupt can crash, reads it.
    let length = unsafe {
        if (&raw const (*record).magic).read_volatile() != CRASH_MAGIC {
            return None;
        }
        (&raw mut (*record).magic).write_volatile(0);
        *buffer = (&raw const (*record).message).read_volatile();
        ((&raw const (*record).length).read_volatile() as usize).min(CRASH_MESSAGE_SIZE)
    };
    Some(match core::str::from_utf8(&buffer[..length]) {
        Ok(message) => message,
        // Cut in the middle of a character: keep what is whole.
        Err(error) => core::str::from_utf8(&buffer[..error.valid_up_to()]).unwrap_or(""),
    })
}

/// Forgets the crashes counted so far: this run stayed up.
pub fn mark_stable() {
    pac::POWER.gpregret2().write(|w| w.0 = 0);
}

#[panic_handler]
fn panic(information: &core::panic::PanicInfo) -> ! {
    crash(format_args!("{information}"))
}

#[cortex_m_rt::exception]
unsafe fn HardFault(frame: &cortex_m_rt::ExceptionFrame) -> ! {
    crash(format_args!(
        "hard fault at {:#010x} (called from {:#010x})",
        frame.pc(),
        frame.lr()
    ))
}

/// An interrupt nothing handles: one the bootloader left pending, or a bug.
#[cortex_m_rt::exception]
unsafe fn DefaultHandler(interrupt: i16) -> ! {
    crash(format_args!("unhandled interrupt {interrupt}"))
}

/// Starts the bring-up fallback: without a host terminal on the USB port
/// after [`FALLBACK_SECONDS`], the board goes back to its bootloader.
#[cfg(feature = "bring-up")]
pub fn start_bootloader_fallback() {
    const FALLBACK_SECONDS: u32 = 60;

    let timer = pac::TIMER1;
    timer.tasks_stop().write_value(1);
    timer.shorts().write(|w| w.0 = 0);
    timer.intenclr().write(|w| w.0 = !0);
    // Timer mode, 32 bits, 16 MHz / 2^4: one tick per microsecond.
    timer.mode().write(|w| w.0 = 0);
    timer.bitmode().write(|w| w.0 = 3);
    timer.prescaler().write(|w| w.0 = 4);
    timer.tasks_clear().write_value(1);
    timer.events_compare(0).write_value(0);
    timer.cc(0).write_value(FALLBACK_SECONDS * 1_000_000);
    // Interrupt on compare 0 only.
    timer.intenset().write(|w| w.0 = 1 << 16);
    timer.tasks_start().write_value(1);
    // SAFETY: the handler below is the only user of this interrupt and shares
    // no data with the rest of the firmware.
    unsafe { cortex_m::peripheral::NVIC::unmask(pac::Interrupt::TIMER1) };
}

#[cfg(feature = "bring-up")]
#[unsafe(no_mangle)]
unsafe extern "C" fn TIMER1() {
    let timer = pac::TIMER1;
    timer.tasks_stop().write_value(1);
    timer.intenclr().write(|w| w.0 = !0);
    timer.events_compare(0).write_value(0);
    if !drivers_nordic::usb_console::host_seen() {
        reset(Reset::Bootloader)
    }
}

/// System power device: an application reset, run by the lifecycle after it
/// committed storage. The board cannot cut its own power or sleep yet.
pub struct BoardPower;

impl peripherals::power::PowerController for BoardPower {
    fn capabilities(&self) -> u8 {
        // `device::power::Capabilities::RESTART`.
        2
    }

    fn restart(&mut self, _status: u8) -> Result<(), peripherals::power::Error> {
        reset(Reset::Application)
    }
}
