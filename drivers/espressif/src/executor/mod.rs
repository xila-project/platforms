#[cfg(feature = "dual-core")]
pub mod dual_core;
mod signal;

use embassy_executor::{Spawner, raw};
use signal::WorkSignal;
use static_cell::StaticCell;

static WORK: WorkSignal = WorkSignal::new();
static EXECUTOR: StaticCell<Executor> = StaticCell::new();

#[unsafe(export_name = "__pender")]
fn pend_executor(context: *mut ()) {
    // SAFETY: every Espressif executor sets its context to its static work flag.
    let work = unsafe { &*context.cast::<WorkSignal>() };
    work.signal();
    #[cfg(feature = "dual-core")]
    dual_core::interrupt_if_remote(work);
}

/// One thread-mode Embassy executor for an Espressif core.
pub struct Executor {
    inner: raw::Executor,
    work: &'static WorkSignal,
}

impl Executor {
    /// Takes the executor from static storage. Panics if it has already been taken.
    pub fn take() -> &'static mut Self {
        EXECUTOR.init(Self::new(&WORK))
    }

    fn new(work: &'static WorkSignal) -> Self {
        Self {
            inner: raw::Executor::new(core::ptr::from_ref(work).cast_mut().cast()),
            work,
        }
    }

    /// Starts the executor in thread mode and never returns.
    pub fn start(&'static mut self, initialize: impl FnOnce(Spawner)) -> ! {
        let spawner = self.inner.spawner();
        let activity = task::metrics::ExecutorActivity::new(spawner);
        initialize(spawner);

        loop {
            self.work.begin_poll();
            // SAFETY: this loop owns the executor and never polls it reentrantly.
            unsafe { self.inner.poll() };
            activity.measure_idle(|| sleep_unless_pending(self.work));
        }
    }
}

// Sleeping without losing a wake-up.
//
// A wake-up that arrives between reading the flag and executing the wait
// instruction must not be lost, so the flag is read with interrupts masked and
// the core sleeps without unmasking them first. An interrupt raised after the
// read stays pending and ends the wait immediately; it is then serviced as
// soon as the previous mask is restored, sets the flag, and the loop polls
// again.
//
// `critical_section::with` must not be held across the wait: esp-hal's
// implementation also takes a lock that interrupt handlers take again
// (reentrantly, which leaves the interrupt level raised for the rest of the
// handler), and on the ESP32-P4 it raises the interrupt threshold so that no
// interrupt can end the wait. Only this core's interrupts are masked here, and
// the whole sequence is one assembly block so that the compiler cannot insert
// code (such as a register push) between the read and the wait.

/// Xtensa: `rsil` raises the interrupt level to 5, the level esp-hal's
/// critical section uses. `waiti 0` lowers it to 0 and sleeps in one
/// instruction, so a pending interrupt is taken at once and ends the wait.
/// `PS` is then restored in both paths.
#[cfg(target_arch = "xtensa")]
fn sleep_unless_pending(work: &WorkSignal) {
    if work.is_pending() {
        return;
    }

    // SAFETY: the sequence only masks interrupts, reads one byte of a live
    // static, optionally sleeps, and restores the previous `PS` value.
    unsafe {
        core::arch::asm!(
            "rsil {state}, 5",
            "l8ui {pending}, {flag}, 0",
            "bnez {pending}, 2f",
            "waiti 0",
            "2:",
            "wsr.ps {state}",
            "rsync",
            state = out(reg) _,
            pending = out(reg) _,
            flag = in(reg) work.flag_address(),
            options(nostack),
        );
    }
}

/// RISC-V: only `mstatus.MIE` is cleared (the interrupt threshold is left
/// unchanged). `wfi` still returns when an enabled interrupt is pending with
/// `MIE` clear; the interrupt is taken when `MIE` is restored.
#[cfg(target_arch = "riscv32")]
fn sleep_unless_pending(work: &WorkSignal) {
    // Like esp-hal's `wait_for_interrupt`, do not sleep while a debugger is
    // attached: unless the wait mode is forced on, the debugger cannot access
    // memory while the core waits. This skips the wait whenever one is attached.
    if work.is_pending() || esp_hal::debugger::debugger_connected() {
        return;
    }

    // SAFETY: the sequence only clears `mstatus.MIE`, reads one byte of a live
    // static, optionally sleeps, and sets `MIE` again only if it was set.
    unsafe {
        core::arch::asm!(
            "csrrci {status}, mstatus, 8",
            "lbu {pending}, 0({flag})",
            "bnez {pending}, 2f",
            "wfi",
            "2:",
            "andi {status}, {status}, 8",
            "csrs mstatus, {status}",
            status = out(reg) _,
            pending = out(reg) _,
            flag = in(reg) work.flag_address(),
            options(nostack),
        );
    }
}
