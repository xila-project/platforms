//! Crash report retention for ESP32-S3.

use core::{
    arch::asm,
    hint::spin_loop,
    ops::Range,
    sync::atomic::{AtomicBool, Ordering, fence},
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

/// Upper bound of the claim spin in the panic path. A core that holds the
/// claim in boot code releases it within microseconds; a core that panicked
/// first never releases it, and the panic path must not spin forever with
/// interrupts disabled.
const CLAIM_SPIN_BOUND: u32 = 1 << 20;

/// Take access to retention for a panic, until reset. The other core may
/// still run: when the claim cannot be taken within the spin bound, the
/// record is written through `Retention::slot_raw` with volatile writes only,
/// never through a second `&mut` to the shared slot.
pub fn claim_for_panic() -> Retention {
    // SAFETY: Panic handling never returns to the interrupted code. Disable
    // maskable interrupts without taking the HAL's cross-core lock.
    unsafe { asm!("wsr {mask}, intenable", "rsync", mask = in(reg) 0u32, options(nostack)) };
    for _ in 0..CLAIM_SPIN_BOUND {
        if try_claim() {
            return Retention {
                panic: true,
                owned: true,
            };
        }
        spin_loop();
    }
    // The other core still holds the claim, for example because it panicked
    // first and never releases it. Proceeding without the claim is acceptable
    // only because this path never creates a `&mut` to the shared statics:
    // two live `&mut` references to one slot would be undefined behavior even
    // though the magic written last detects the torn record on the next boot.
    Retention {
        panic: true,
        owned: false,
    }
}

/// Take temporary exclusive access to retention in normal boot code.
pub fn try_claim_at_boot() -> Option<Retention> {
    try_claim().then_some(Retention {
        panic: false,
        owned: true,
    })
}

fn try_claim() -> bool {
    CLAIMED
        .compare_exchange(false, true, Ordering::Acquire, Ordering::Relaxed)
        .is_ok()
}

pub struct Retention {
    panic: bool,
    owned: bool,
}

impl Retention {
    /// Whether this token owns the claim and may use `slot` and `guard`.
    pub fn owns_claim(&self) -> bool {
        self.owned
    }

    /// Exclusive access to the retained slot, only when the claim is owned.
    pub fn slot(&mut self) -> Option<&mut [u8; ESPRESSIF_RECORD_CAPACITY]> {
        if self.owned {
            // SAFETY: The claim gives this token exclusive access to the slot:
            // no other token can hold a reference to it.
            Some(unsafe { &mut *(&raw mut RETAINED_RECORD) })
        } else {
            None
        }
    }

    /// The retained slot as a raw pointer, for the panic path that could not
    /// take the claim. The record must be written through this pointer with
    /// volatile writes only (`copy_record`): the core that holds the claim may
    /// have a live `&mut` to the same slot, and a second `&mut` would be
    /// undefined behavior.
    pub fn slot_raw(&mut self) -> *mut [u8; ESPRESSIF_RECORD_CAPACITY] {
        &raw mut RETAINED_RECORD
    }

    /// Record a crash in the retained guard and return the new count. Without
    /// the claim, the guard is updated through a local copy and volatile reads
    /// and writes, so no second `&mut` to the shared guard is created either.
    pub fn record_crash(&mut self) -> u32 {
        if self.owned {
            self.guard().record_crash()
        } else {
            let mut local = [0; BOOT_GUARD_SIZE];
            // SAFETY: Volatile byte reads within the retained guard.
            unsafe { read_volatile_bytes((&raw mut BOOT_GUARD).cast::<u8>(), &mut local) };
            let count = BootGuard::new(&mut local).record_crash();
            // SAFETY: Volatile byte writes within the retained guard; a torn
            // update is rejected by the guard's magic and checksum.
            unsafe { write_volatile_bytes((&raw mut BOOT_GUARD).cast::<u8>(), &local) };
            count
        }
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

/// Copy bytes from a raw address into a local buffer with volatile reads.
///
/// # Safety
/// The range starting at `source` for `output.len()` bytes must be readable.
unsafe fn read_volatile_bytes(source: *const u8, output: &mut [u8]) {
    for (offset, byte) in output.iter_mut().enumerate() {
        // SAFETY: The caller guarantees the readable range.
        unsafe { *byte = core::ptr::read_volatile(source.add(offset)) };
    }
}

/// Copy bytes to a raw address with volatile writes, then fence.
///
/// # Safety
/// The range starting at `destination` for `source.len()` bytes must be
/// writable, and no `&mut` to it may be live on any core.
unsafe fn write_volatile_bytes(destination: *mut u8, source: &[u8]) {
    for (offset, byte) in source.iter().enumerate() {
        // SAFETY: The caller guarantees the writable range and the absence of
        // a live `&mut`; the volatile write keeps the compiler from assuming
        // exclusive access.
        unsafe { core::ptr::write_volatile(destination.add(offset), *byte) };
    }
    fence(Ordering::SeqCst);
}

/// Copy a finished record into the retained slot, magic last, with volatile
/// writes only.
///
/// # Safety
/// `destination` must be the retained record slot, `source` a complete record,
/// and no `&mut` to the slot may be live on any core: the claim holder may
/// write through one, and two `&mut` references to the same memory would be
/// undefined behavior. A record torn by a concurrent write is detected by the
/// magic and the checksum on the next boot.
pub unsafe fn copy_record(destination: *mut [u8; ESPRESSIF_RECORD_CAPACITY], source: &[u8]) {
    let destination = destination.cast::<u8>();
    let length = source.len().min(ESPRESSIF_RECORD_CAPACITY);
    let record = &source[..length];
    if length > 4 {
        // SAFETY: The caller guarantees a writable slot and no live `&mut`.
        unsafe { write_volatile_bytes(destination.add(4), &record[4..]) };
        // SAFETY: The magic goes last, so a present magic means a full record.
        unsafe { write_volatile_bytes(destination, &record[..4]) };
    } else {
        // SAFETY: The caller guarantees a writable slot and no live `&mut`.
        unsafe { write_volatile_bytes(destination, record) };
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

// esp-hal 1.2.2's linker script defines only the core 0 stack bounds. Define
// the core 1 bounds as weak zeroes: a linker script that provides them with
// strong definitions overrides these, and a zero address then means that the
// symbols were not provided.
core::arch::global_asm!(
    ".weak _stack_start_cpu1",
    ".weak _stack_end_cpu1",
    ".set _stack_start_cpu1, 0",
    ".set _stack_end_cpu1, 0",
);

pub fn main_stack_bounds() -> Range<usize> {
    unsafe extern "C" {
        static _stack_end_cpu0: u8;
        static _stack_start_cpu0: u8;
        static _stack_end_cpu1: u8;
        static _stack_start_cpu1: u8;
    }
    let (end, start) = if Cpu::current() == Cpu::ProCpu {
        (
            (&raw const _stack_end_cpu0).addr(),
            (&raw const _stack_start_cpu0).addr(),
        )
    } else {
        (
            (&raw const _stack_end_cpu1).addr(),
            (&raw const _stack_start_cpu1).addr(),
        )
    };
    // A zero bound means the linker script does not provide this core's stack
    // symbols (esp-hal 1.2.2 provides only the core 0 ones).
    if end == 0 || start == 0 || end >= start {
        return 0..0;
    }
    end..start
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
