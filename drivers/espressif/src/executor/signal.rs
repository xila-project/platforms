use core::sync::atomic::{AtomicBool, Ordering};

/// Records that a task or an interrupt asked the executor to poll again.
///
/// Only loads and stores are used, never compare-and-swap, so it works on the
/// chips without atomic read-modify-write instructions. No update is lost:
/// interrupts only ever set the flag, and the executor clears it only right
/// before polling, so a request made after the clear is either handled by that
/// poll or still visible when the executor decides whether to sleep.
pub(super) struct WorkSignal(AtomicBool);

impl WorkSignal {
    pub(super) const fn new() -> Self {
        Self(AtomicBool::new(false))
    }

    /// Requests another poll. Callable from any context, including interrupts.
    pub(super) fn signal(&self) {
        self.0.store(true, Ordering::SeqCst);
    }

    /// Clears the request before polling: work requested from now on needs
    /// another poll.
    pub(super) fn begin_poll(&self) {
        self.0.store(false, Ordering::SeqCst);
    }

    /// Returns whether work was requested since the last `begin_poll`.
    pub(super) fn is_pending(&self) -> bool {
        self.0.load(Ordering::SeqCst)
    }

    /// Address of the flag (one byte, nonzero when pending), for the idle
    /// sequence that must read it with interrupts masked inside a single
    /// assembly block.
    pub(super) fn flag_address(&self) -> *const u8 {
        self.0.as_ptr().cast_const().cast()
    }
}
