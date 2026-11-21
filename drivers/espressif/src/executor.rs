use core::cell::Cell;

use critical_section::Mutex;
use embassy_executor::{Spawner, raw};
use static_cell::StaticCell;

static WORK_PENDING: Mutex<Cell<bool>> = Mutex::new(Cell::new(false));
static EXECUTOR: StaticCell<Executor> = StaticCell::new();

#[unsafe(export_name = "__pender")]
fn pend_executor(_context: *mut ()) {
    critical_section::with(|section| WORK_PENDING.borrow(section).set(true));
}

/// Single-core Embassy executor for Espressif targets.
pub struct Executor {
    inner: raw::Executor,
}

impl Executor {
    /// Takes the executor from static storage. Panics if it has already been taken.
    pub fn take() -> &'static mut Self {
        EXECUTOR.init(Self {
            inner: raw::Executor::new(core::ptr::null_mut()),
        })
    }

    /// Starts the executor in thread mode and never returns.
    ///
    /// All interrupts and tasks that wake the executor must run on this core.
    /// Keep the other application core parked while using this executor.
    pub fn start(&'static mut self, initialize: impl FnOnce(Spawner)) -> ! {
        initialize(self.inner.spawner());

        loop {
            // SAFETY: this loop owns the executor and never polls it reentrantly.
            unsafe { self.inner.poll() };
            critical_section::with(|section| {
                if !WORK_PENDING.borrow(section).replace(false) {
                    // Mask interrupts between checking for work and sleeping.
                    // The HAL uses WAITI 0 on Xtensa and WFI on RISC-V.
                    esp_hal::interrupt::wait_for_interrupt();
                }
            });
        }
    }
}
