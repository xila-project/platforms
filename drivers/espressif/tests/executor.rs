#[path = "../src/executor/signal.rs"]
mod signal;

use signal::WorkSignal;

fn flag(work: &WorkSignal) -> u8 {
    // SAFETY: the address points to the live flag of `work`.
    unsafe { work.flag_address().read_volatile() }
}

#[test]
fn work_signalled_during_a_poll_prevents_sleeping() {
    let work = WorkSignal::new();
    work.begin_poll();
    work.signal();
    assert!(work.is_pending());
    assert_ne!(flag(&work), 0);
}

#[test]
fn work_signalled_before_a_poll_is_handled_by_that_poll() {
    let work = WorkSignal::new();
    work.signal();
    work.begin_poll();
    assert!(!work.is_pending());
    assert_eq!(flag(&work), 0);
}

#[test]
fn repeated_signals_need_a_single_poll() {
    let work = WorkSignal::new();
    work.signal();
    work.signal();
    assert!(work.is_pending());
    work.begin_poll();
    assert!(!work.is_pending());
}
