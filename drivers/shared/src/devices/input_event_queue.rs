//! Queueing input events without ever logging their content.
//!
//! A full keyboard queue used to be reported together with the rejected event,
//! which carries the typed or pasted character and so could leak part of a
//! password into the log. Only the number of dropped events is reported here.

use core::sync::atomic::{AtomicU32, Ordering};
use synchronization::{blocking_mutex::raw::CriticalSectionRawMutex, channel::Sender};

static DROPPED_INPUT_EVENTS: AtomicU32 = AtomicU32::new(0);

/// Number of input events dropped so far because a queue was full.
pub fn dropped_input_events() -> u32 {
    DROPPED_INPUT_EVENTS.load(Ordering::Relaxed)
}

/// Queue an input event, counting it instead of logging it when the queue is full.
///
/// Returns whether the event was queued. The drop is reported to the log on the
/// first drop and then every time the total reaches a power of two, so a stuck
/// consumer cannot flood the log. The report holds the running total and never
/// the event.
pub fn send_input_event<T, const N: usize>(
    sender: &Sender<'_, CriticalSectionRawMutex, T, N>,
    event: T,
) -> bool {
    if sender.try_send(event).is_ok() {
        return true;
    }

    let total = DROPPED_INPUT_EVENTS
        .fetch_add(1, Ordering::Relaxed)
        .wrapping_add(1);

    if total.is_power_of_two() {
        log::error!("Input queue full: {total} events dropped so far");
    }

    false
}
