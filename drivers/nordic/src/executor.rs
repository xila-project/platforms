use core::sync::atomic::{AtomicBool, Ordering};
use embassy_executor::{Spawner, raw};

/// Single-threaded Cortex-M executor using WFE/SEV for power-efficient idle waiting.
pub struct Executor {
    inner: raw::Executor,
    stop: AtomicBool,
}

impl Default for Executor {
    fn default() -> Self {
        Self::new()
    }
}

impl Executor {
    /// Create a new Executor.
    pub fn new() -> Self {
        // In embassy-executor for Cortex-M thread mode, THREAD_PENDER is usize::MAX.
        Self {
            inner: raw::Executor::new(usize::MAX as *mut ()),
            stop: AtomicBool::new(false),
        }
    }

    /// Stop the executor loop when running via `run`.
    pub fn stop(&self) {
        self.stop.store(true, Ordering::SeqCst);
        cortex_m::asm::sev();
    }

    /// Get a spawner for this executor.
    pub fn spawner(&'static self) -> Spawner {
        self.inner.spawner()
    }

    /// Run the executor until stopped.
    pub fn run(&'static self, init: impl FnOnce(Spawner, &'static Self)) {
        drivers_shared::executor::run_until_stopped(
            &self.stop,
            || init(self.inner.spawner(), self),
            || self.poll_and_wait(),
        );
    }

    /// Start the executor loop; does not return.
    pub fn start(&'static mut self, init: impl FnOnce(Spawner)) -> ! {
        let executor: &'static Self = self;

        drivers_shared::executor::run_forever(
            || init(executor.inner.spawner()),
            || executor.poll_and_wait(),
        )
    }

    /// Poll the executor, then sleep until an event (`sev` from a wake-up or
    /// from [`Executor::stop`], or an interrupt) arrives.
    fn poll_and_wait(&'static self) {
        unsafe {
            self.inner.poll();
            cortex_m::asm::wfe();
        }
    }
}

/// Share the static storage of the executor with the other drivers.
#[doc(hidden)]
pub use drivers_shared as __shared;

#[macro_export]
macro_rules! instantiate_static_executor {
    () => {
        $crate::executor::__shared::instantiate_static_executor!($crate::executor::Executor)
    };
}

pub use instantiate_static_executor;
