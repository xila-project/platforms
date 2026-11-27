//! Crash report retention for ESP32-S3.

use core::{
    arch::asm,
    hint::spin_loop,
    ops::Range,
    sync::atomic::{AtomicBool, Ordering},
};

use ::crash::{BOOT_GUARD_SIZE, BootGuard, ESPRESSIF_RECORD_CAPACITY};
use esp_hal::{
    rtc_cntl::SocResetReason,
    system::{Cpu, reset_reason},
    timer::systimer::{SystemTimer, Unit},
};

#[esp_hal::ram(unstable(rtc_slow, persistent))]
static mut RETAINED_RECORD: [u8; ESPRESSIF_RECORD_CAPACITY] = [0; ESPRESSIF_RECORD_CAPACITY];

#[esp_hal::ram(unstable(rtc_slow, persistent))]
static mut BOOT_GUARD: [u8; BOOT_GUARD_SIZE] = [0; BOOT_GUARD_SIZE];

static CLAIMED: AtomicBool = AtomicBool::new(false);

/// Take exclusive access to retention until reset. The other core may still run.
pub fn claim_for_panic() -> Retention {
    // SAFETY: Panic handling never returns to the interrupted code. Disable
    // maskable interrupts without taking the HAL's cross-core lock.
    unsafe { asm!("wsr {mask}, intenable", "rsync", mask = in(reg) 0u32, options(nostack)) };
    while !try_claim() {
        spin_loop();
    }
    Retention { panic: true }
}

/// Take temporary exclusive access to retention in normal boot code.
pub fn try_claim_at_boot() -> Option<Retention> {
    try_claim().then_some(Retention { panic: false })
}

fn try_claim() -> bool {
    CLAIMED
        .compare_exchange(false, true, Ordering::Acquire, Ordering::Relaxed)
        .is_ok()
}

pub struct Retention {
    panic: bool,
}

impl Retention {
    pub fn slot(&mut self) -> &mut [u8; ESPRESSIF_RECORD_CAPACITY] {
        // SAFETY: The claim gives this token exclusive access to the slot.
        unsafe { &mut *(&raw mut RETAINED_RECORD) }
    }

    pub fn guard(&mut self) -> BootGuard<'_> {
        // SAFETY: The claim gives this token exclusive access to the guard.
        BootGuard::new(unsafe { &mut *(&raw mut BOOT_GUARD) })
    }
}

impl Drop for Retention {
    fn drop(&mut self) {
        if !self.panic {
            CLAIMED.store(false, Ordering::Release);
        }
    }
}

pub fn uptime_microseconds() -> u64 {
    // ESP32-S3's 40 MHz crystal clocks the system timer at 16 MHz.
    SystemTimer::unit_value(Unit::Unit0) / 16
}

pub fn current_reset_reason() -> (::crash::ResetReason, u32) {
    use ::crash::ResetReason;
    let reason = reset_reason();
    let normalized = match reason {
        Some(SocResetReason::ChipPowerOn) => ResetReason::PowerOn,
        Some(SocResetReason::CoreSw | SocResetReason::CpuSw) => ResetReason::SoftwareReset,
        Some(
            SocResetReason::CoreMwdt0
            | SocResetReason::CoreMwdt1
            | SocResetReason::CoreRtcWdt
            | SocResetReason::CpuMwdt0
            | SocResetReason::CpuMwdt1
            | SocResetReason::CpuRtcWdt
            | SocResetReason::SysRtcWdt
            | SocResetReason::SysSuperWdt,
        ) => ResetReason::Watchdog,
        Some(SocResetReason::SysBrownOut) => ResetReason::BrownOut,
        _ => ResetReason::Unknown,
    };
    (normalized, reason.map_or(0, |reason| reason as u32))
}

pub struct Registers {
    pub stack_pointer: usize,
    pub link_register: usize,
}

impl Registers {
    /// Capture the current register window, then spill older windows to RAM.
    #[inline(always)]
    pub fn capture() -> Self {
        let stack_pointer;
        let link_register;
        // SAFETY: The rotations visit all windows and return to the initial
        // one. Referencing a12 forces the window overflow handler to spill.
        // See esp-backtrace's Xtensa register-window walk.
        unsafe {
            asm!(
                "mov {stack}, a1",
                "mov {link}, a0",
                "and a12, a12, a12", "rotw 3",
                "and a12, a12, a12", "rotw 3",
                "and a12, a12, a12", "rotw 3",
                "and a12, a12, a12", "rotw 3",
                "and a12, a12, a12", "rotw 4",
                stack = out(reg) stack_pointer,
                link = out(reg) link_register,
            );
        }
        Self {
            stack_pointer,
            link_register: normalize_return_address(link_register),
        }
    }
}

fn normalize_return_address(address: usize) -> usize {
    if address & 0x3fff_ffff == 0 {
        0
    } else {
        (address & 0x3fff_ffff) | 0x4000_0000
    }
}

pub fn main_stack_bounds() -> Range<usize> {
    unsafe extern "C" {
        static _stack_end_cpu0: u8;
        static _stack_start_cpu0: u8;
    }
    if Cpu::current() != Cpu::ProCpu {
        return 0..0;
    }
    (&raw const _stack_end_cpu0).addr()..(&raw const _stack_start_cpu0).addr()
}

/// Read the return address and previous stack pointer from the base save area.
/// A frame must lie in the active stack and its caller must move toward the end.
///
/// # Safety
/// The bounds must cover readable memory on the calling core's active stack.
pub unsafe fn caller(stack_pointer: usize, bounds: &Range<usize>) -> Option<(usize, usize)> {
    let base = stack_pointer.checked_sub(16)?;
    if base < bounds.start || stack_pointer > bounds.end || !stack_pointer.is_multiple_of(16) {
        return None;
    }

    let address: usize;
    let parent: usize;
    // SAFETY: The aligned pair is inside the established readable stack.
    // Assembly reads the saved words as hardware values, including padding.
    unsafe {
        asm!(
            "l32i {address}, {base}, 0",
            "l32i {parent}, {base}, 4",
            base = in(reg) base,
            address = out(reg) address,
            parent = out(reg) parent,
            options(readonly, nostack),
        );
    }
    let address = normalize_return_address(address);
    if address == 0 || parent <= stack_pointer || parent > bounds.end {
        return None;
    }
    Some((address - 3, parent))
}

/// Copy stack bytes into the record using byte loads.
///
/// # Safety
/// The range starting at `address` for `output.len()` bytes must be readable.
pub unsafe fn copy_stack(address: usize, output: &mut [u8]) {
    for (offset, byte) in output.iter_mut().enumerate() {
        let value: u32;
        // SAFETY: The caller established the range. Assembly can read padding.
        unsafe {
            asm!(
                "l8ui {value}, {address}, 0",
                address = in(reg) address + offset,
                value = out(reg) value,
                options(readonly, nostack),
            );
        }
        *byte = value as u8;
    }
}
