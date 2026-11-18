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
        init(self.inner.spawner(), self);

        while !self.stop.load(Ordering::SeqCst) {
            unsafe {
                self.inner.poll();
                cortex_m::asm::wfe();
            }
        }
    }

    /// Start the executor loop; does not return.
    pub fn start(&'static mut self, init: impl FnOnce(Spawner)) -> ! {
        init(self.inner.spawner());

        loop {
            unsafe {
                self.inner.poll();
                cortex_m::asm::wfe();
            }
        }
    }
}

#[macro_export]
macro_rules! instantiate_static_executor {
    () => {{
        static mut __EXECUTOR: Option<$crate::executor::Executor> = None;

        unsafe {
            if __EXECUTOR.is_none() {
                __EXECUTOR = Some($crate::executor::Executor::new());
            }
            __EXECUTOR.as_mut().expect("Executor is not initialized")
        }
    }};
}

pub use instantiate_static_executor;
