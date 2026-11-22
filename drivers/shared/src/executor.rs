//! Executor startup shared by the platform drivers.
//!
//! The drivers own what differs between platforms: how an executor is woken
//! (a Cortex-M `sev`, a thread condition variable, the browser event loop, an
//! Espressif work flag) and how an idle core waits. This module owns what they
//! used to copy: the static storage of the executor and the order of the
//! startup sequence (initial work first, then poll and wait until stopped).

use core::sync::atomic::{AtomicBool, Ordering};

/// Run `initialize`, then `poll_and_wait` until `stop` is set.
///
/// `initialize` runs once, before the first poll, so the work it spawns is
/// polled first. `poll_and_wait` is the platform step: it polls the executor,
/// then waits for the next wake-up. `stop` is checked before every step,
/// including the first, so a stop requested by `initialize` skips every poll,
/// and one requested later (followed by the platform's wake-up) ends the loop
/// after the step that was waiting.
pub fn run_until_stopped(
    stop: &AtomicBool,
    initialize: impl FnOnce(),
    mut poll_and_wait: impl FnMut(),
) {
    initialize();

    while !stop.load(Ordering::SeqCst) {
        poll_and_wait();
    }
}

/// Run `initialize`, then `poll_and_wait` forever.
pub fn run_forever(initialize: impl FnOnce(), mut poll_and_wait: impl FnMut()) -> ! {
    initialize();

    loop {
        poll_and_wait();
    }
}

/// Create the executor in static storage on first use and return it.
///
/// The executor type must provide `new()`. Every use of the macro owns its own
/// storage, so each expansion site gets its own executor. Like the executors'
/// own `start`, this hands out a `&'static mut`: call it once per executor.
#[macro_export]
macro_rules! instantiate_static_executor {
    ($executor:ty) => {{
        static mut __EXECUTOR: Option<$executor> = None;

        // SAFETY: the storage is private to this expansion and only reached
        // through this raw pointer.
        unsafe {
            (&mut *::core::ptr::addr_of_mut!(__EXECUTOR)).get_or_insert_with(<$executor>::new)
        }
    }};
}

#[cfg(test)]
mod tests {
    use super::*;
    use core::cell::{Cell, RefCell};
    use std::vec::Vec;

    #[test]
    fn initial_work_runs_before_the_first_poll() {
        let events = RefCell::new(Vec::new());
        let stop = AtomicBool::new(false);

        run_until_stopped(
            &stop,
            || events.borrow_mut().push("initialize"),
            || {
                events.borrow_mut().push("poll");
                stop.store(true, Ordering::SeqCst);
            },
        );

        assert_eq!(*events.borrow(), ["initialize", "poll"]);
    }

    #[test]
    fn keeps_polling_until_stopped() {
        let polls = Cell::new(0);
        let stop = AtomicBool::new(false);

        run_until_stopped(
            &stop,
            || {},
            || {
                polls.set(polls.get() + 1);
                if polls.get() == 3 {
                    stop.store(true, Ordering::SeqCst);
                }
            },
        );

        assert_eq!(polls.get(), 3);
    }

    #[test]
    fn a_stop_requested_during_initialization_skips_every_poll() {
        let polls = Cell::new(0);
        let stop = AtomicBool::new(false);

        run_until_stopped(
            &stop,
            || stop.store(true, Ordering::SeqCst),
            || polls.set(polls.get() + 1),
        );

        assert_eq!(polls.get(), 0);
    }

    #[test]
    fn a_wake_up_is_not_lost_between_two_steps() {
        // The platform step reports the wake-ups it consumed: one that arrives
        // while the executor is polling is seen by the next step.
        let pending_wake_ups = Cell::new(1);
        let handled = Cell::new(0);
        let stop = AtomicBool::new(false);

        run_until_stopped(
            &stop,
            || {},
            || {
                handled.set(handled.get() + pending_wake_ups.replace(0));
                if handled.get() == 1 {
                    // Woken again while polling.
                    pending_wake_ups.set(1);
                } else {
                    stop.store(true, Ordering::SeqCst);
                }
            },
        );

        assert_eq!(handled.get(), 2);
    }

    #[test]
    fn static_storage_creates_the_executor_once_per_site() {
        struct Counter {
            created: usize,
        }

        static CREATIONS: core::sync::atomic::AtomicUsize = core::sync::atomic::AtomicUsize::new(0);

        impl Counter {
            fn new() -> Self {
                Self {
                    created: CREATIONS.fetch_add(1, Ordering::SeqCst) + 1,
                }
            }
        }

        fn take() -> &'static mut Counter {
            instantiate_static_executor!(Counter)
        }

        assert_eq!(take().created, 1);
        // A second use of the same site reuses the storage.
        assert_eq!(take().created, 1);
        assert_eq!(CREATIONS.load(Ordering::SeqCst), 1);
    }
}
