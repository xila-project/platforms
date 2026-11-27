use embassy_executor::{Spawner, raw};
use std::boxed::Box;
use std::marker::PhantomData;
use std::sync::atomic::AtomicBool;
use std::sync::{Condvar, Mutex};
use synchronization::blocking_mutex::raw::CriticalSectionRawMutex;
use synchronization::signal::Signal;
use task::SpawnerIdentifier;

/// Single-threaded std-based executor.
pub struct Executor {
    inner: raw::Executor,
    not_send: PhantomData<*mut ()>,
    signaler: &'static Signaler,
    stop: AtomicBool,
    #[cfg(all(feature = "profiling", target_os = "linux"))]
    profiling: std::sync::Arc<crate::profiling::ExecutorCounters>,
}

impl Default for Executor {
    fn default() -> Self {
        Self::new()
    }
}

impl Executor {
    /// Create a new Executor.
    pub fn new() -> Self {
        let signaler = Box::leak(Box::new(Signaler::new()));
        Self {
            inner: raw::Executor::new(signaler as *mut Signaler as *mut ()),
            not_send: PhantomData,
            signaler,
            stop: AtomicBool::new(false),
            #[cfg(all(feature = "profiling", target_os = "linux"))]
            profiling: crate::profiling::ExecutorCounters::new(),
        }
    }

    pub fn new_static() -> &'static Self {
        Box::leak(Box::new(Self::new()))
    }

    pub fn stop(&self) {
        self.stop.store(true, std::sync::atomic::Ordering::SeqCst);
        self.signaler.signal();
    }

    /// Get a spawner for this executor.
    pub fn spawner(&'static self) -> Spawner {
        self.inner.spawner()
    }

    /// Run the executor.
    ///
    /// The `init` closure is called with a [`Spawner`] that spawns tasks on
    /// this executor. Use it to spawn the initial task(s). After `init` returns,
    /// the executor starts running the tasks.
    ///
    /// To spawn more tasks later, you may keep copies of the [`Spawner`] (it is `Copy`),
    /// for example by passing it as an argument to the initial tasks.
    ///
    /// This function requires `&'static mut self`. This means you have to store the
    /// Executor instance in a place where it'll live forever and grants you mutable
    /// access. There's a few ways to do this:
    ///
    /// - a [StaticCell](https://docs.rs/static_cell/latest/static_cell/) (safe)
    /// - a `static mut` (unsafe)
    /// - a local variable in a function you know never returns (like `fn main() -> !`), upgrading its lifetime with `transmute`. (unsafe)
    ///
    /// This function never returns.
    pub fn run(&'static self, init: impl FnOnce(Spawner, &'static Self)) {
        drivers_shared::executor::run_until_stopped(
            &self.stop,
            || init(self.inner.spawner(), self),
            || self.poll_and_wait(),
        );
    }

    #[inline(always)]
    fn poll_and_wait(&'static self) {
        #[cfg(all(feature = "profiling", target_os = "linux"))]
        {
            self.profiling.poll(|| unsafe { self.inner.poll() });
            self.profiling.wait(|| self.signaler.wait());
        }
        #[cfg(not(all(feature = "profiling", target_os = "linux")))]
        {
            unsafe { self.inner.poll() };
            self.signaler.wait();
        }
    }

    pub fn start(&'static mut self, init: impl FnOnce(Spawner)) -> ! {
        let executor: &'static Self = self;

        drivers_shared::executor::run_forever(
            || init(executor.inner.spawner()),
            || executor.poll_and_wait(),
        )
    }
}

struct Signaler {
    mutex: Mutex<bool>,
    condvar: Condvar,
}

impl Signaler {
    fn new() -> Self {
        Self {
            mutex: Mutex::new(false),
            condvar: Condvar::new(),
        }
    }

    fn wait(&self) {
        let mut signaled = self.mutex.lock().unwrap();
        while !*signaled {
            signaled = self.condvar.wait(signaled).unwrap();
        }
        *signaled = false;
    }

    fn signal(&self) {
        let mut signaled = self.mutex.lock().unwrap();
        *signaled = true;
        self.condvar.notify_one();
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

pub async fn new_thread_executor(task_manager: &'static task::Manager) -> SpawnerIdentifier {
    // Create a new OnceLock for each call to allow multiple thread executors
    let signal: &'static Signal<CriticalSectionRawMutex, SpawnerIdentifier> =
        Box::leak(Box::new(Signal::new()));

    std::thread::spawn(move || {
        #[cfg(all(feature = "crash", unix))]
        let _crash_stack = crate::crash::install_executor_stack();
        // Use Box::leak to create a 'static reference for this thread's executor
        let executor = Box::leak(Box::new(Executor::new()));

        executor.start(move |spawner: Spawner| {
            let spawner_id = task_manager.register_spawner(spawner).unwrap();

            signal.signal(spawner_id);
        });
    });

    let spawner_id = signal.wait().await;

    unsafe {
        let _ = Box::from_raw(
            signal as *const _ as *mut Signal<CriticalSectionRawMutex, SpawnerIdentifier>,
        );
        // Clean up the leaked Box to avoid memory leak
    }

    spawner_id
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::Duration;

    static STEPS: AtomicUsize = AtomicUsize::new(0);
    static WAKE: Signal<CriticalSectionRawMutex, ()> = Signal::new();

    #[embassy_executor::task]
    async fn worker(executor: &'static Executor) {
        // Initial work: polled before the executor first waits.
        STEPS.store(1, Ordering::SeqCst);

        WAKE.wait().await;
        STEPS.store(2, Ordering::SeqCst);

        executor.stop();
    }

    /// Initial work runs, the executor then idles without progress, a wake-up
    /// resumes it, and `stop` ends `run`.
    #[test]
    fn runs_initial_work_idles_wakes_and_stops() {
        // The executor is not `Send`: it is created on the thread that runs it.
        let thread = std::thread::spawn(|| {
            Executor::new_static()
                .run(|spawner, executor| spawner.spawn(worker(executor).unwrap()));
        });

        while STEPS.load(Ordering::SeqCst) < 1 {
            std::thread::sleep(Duration::from_millis(1));
        }

        // Idle: nothing wakes the task, so it makes no further progress.
        std::thread::sleep(Duration::from_millis(50));
        assert_eq!(STEPS.load(Ordering::SeqCst), 1);
        assert!(!thread.is_finished());

        WAKE.signal(());
        thread.join().unwrap();
        assert_eq!(STEPS.load(Ordering::SeqCst), 2);
    }

    #[test]
    fn stop_before_run_skips_polling() {
        let executor = Executor::new_static();
        executor.stop();

        let initialized = std::sync::atomic::AtomicBool::new(false);
        executor.run(|_, _| initialized.store(true, Ordering::SeqCst));

        // Initialization still runs; the loop exits at once.
        assert!(initialized.load(Ordering::SeqCst));
    }
}
