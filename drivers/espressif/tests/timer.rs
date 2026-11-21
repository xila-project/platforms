#[path = "../src/time/counter.rs"]
mod counter;

use counter::{Counter, alarm_has_elapsed};

#[test]
fn counter_keeps_fractional_ticks_across_reads_and_wrap() {
    let mut counter = Counter::new(30, 31, 16_000_000);
    assert_eq!(counter.microseconds(31), 0);
    assert_eq!(counter.microseconds(13), 0);
    assert_eq!(counter.microseconds(14), 1);
    assert_eq!(counter.microseconds(30), 2);
}

#[test]
fn alarm_must_be_rearmed_if_its_delay_elapsed_during_programming() {
    assert!(!alarm_has_elapsed(100, 1, 100));
    assert!(alarm_has_elapsed(100, 1, 101));
    assert!(alarm_has_elapsed(100, 1, 105));
}
