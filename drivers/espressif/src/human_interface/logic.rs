//! Transport-independent logic of the USB human interface lower half: the bounded
//! report queue between `HumanInterfaceController::send_report` and the USB runner
//! task, the report framing (the identifier byte in front of the report bytes) and
//! the parsing of the keyboard LED output report.
//!
//! Host-tested by `tests/human_interface.rs`.

use peripherals::human_interface::{
    Error, REPORT_IDENTIFIER_CONSUMER, REPORT_IDENTIFIER_KEYBOARD, REPORT_IDENTIFIER_MOUSE,
    REPORT_IDENTIFIER_SYSTEM, REPORT_MAX_BYTES,
};

/// Number of reports the queue holds between `send_report` and the USB runner task.
pub const REPORT_QUEUE_CAPACITY: usize = 16;

/// Mask of the keyboard LED bits the emulation contract reports: num lock, caps
/// lock and scroll lock, in the order of the LED usages of `REPORT_DESCRIPTOR`.
pub const LED_INDICATOR_MASK: u8 = 0x07;

/// Whether `identifier` is one of the `REPORT_IDENTIFIER_*` constants.
pub const fn is_known_identifier(identifier: u8) -> bool {
    matches!(
        identifier,
        REPORT_IDENTIFIER_KEYBOARD
            | REPORT_IDENTIFIER_CONSUMER
            | REPORT_IDENTIFIER_MOUSE
            | REPORT_IDENTIFIER_SYSTEM
    )
}

/// Copies `report` into a zero-padded array of [`REPORT_MAX_BYTES`] bytes.
fn padded(report: &[u8]) -> [u8; REPORT_MAX_BYTES] {
    let mut bytes = [0u8; REPORT_MAX_BYTES];
    bytes[..report.len()].copy_from_slice(report);
    bytes
}

/// One queued report: its identifier and its bytes without the identifier byte.
#[derive(Clone, Copy)]
struct Slot {
    identifier: u8,
    length: usize,
    bytes: [u8; REPORT_MAX_BYTES],
}

impl Slot {
    const EMPTY: Self = Self {
        identifier: 0,
        length: 0,
        bytes: [0; REPORT_MAX_BYTES],
    };
}

/// A bounded FIFO of reports. `send_report` pushes into it and fails with
/// [`Error::Busy`] when it is full, so no report is dropped silently; the USB runner
/// task pops from it.
pub struct ReportQueue<const N: usize> {
    slots: [Slot; N],
    /// Index of the oldest queued report.
    head: usize,
    count: usize,
}

impl<const N: usize> ReportQueue<N> {
    /// An empty queue.
    pub const fn new() -> Self {
        Self {
            slots: [Slot::EMPTY; N],
            head: 0,
            count: 0,
        }
    }

    /// Queue one report. Fails with [`Error::Busy`] when the queue is full, in which
    /// case the report was not accepted, and with [`Error::Other`] for an unknown
    /// report identifier or a report longer than [`REPORT_MAX_BYTES`].
    pub fn push(&mut self, identifier: u8, report: &[u8]) -> Result<(), Error> {
        if !is_known_identifier(identifier) || report.len() > REPORT_MAX_BYTES {
            return Err(Error::Other);
        }
        if self.count == N {
            return Err(Error::Busy);
        }
        let index = (self.head + self.count) % N;
        self.slots[index] = Slot {
            identifier,
            length: report.len(),
            bytes: padded(report),
        };
        self.count += 1;
        Ok(())
    }

    /// Take the oldest queued report: its identifier, its zero-padded bytes and its
    /// length. `None` when the queue is empty.
    pub fn pop(&mut self) -> Option<(u8, [u8; REPORT_MAX_BYTES], usize)> {
        if self.count == 0 {
            return None;
        }
        let slot = self.slots[self.head];
        self.head = (self.head + 1) % N;
        self.count -= 1;
        Some((slot.identifier, slot.bytes, slot.length))
    }
}

/// Frames one report for the interrupt IN endpoint: the report identifier byte
/// followed by the report bytes, because [`peripherals::human_interface::REPORT_DESCRIPTOR`]
/// declares report identifiers. Returns the frame and its length, or `None` for an
/// unknown identifier or a report longer than [`REPORT_MAX_BYTES`].
pub fn frame_report(identifier: u8, report: &[u8]) -> Option<([u8; 1 + REPORT_MAX_BYTES], usize)> {
    if !is_known_identifier(identifier) || report.len() > REPORT_MAX_BYTES {
        return None;
    }
    let mut frame = [0u8; 1 + REPORT_MAX_BYTES];
    frame[0] = identifier;
    frame[1..1 + report.len()].copy_from_slice(report);
    Some((frame, 1 + report.len()))
}

/// Keyboard LED bitmask from an output report read on the interrupt OUT endpoint,
/// which carries the report identifier byte first. `None` for any other report.
pub fn led_indicators_from_output_report(report: &[u8]) -> Option<u8> {
    match report.split_first() {
        Some((&REPORT_IDENTIFIER_KEYBOARD, payload)) if !payload.is_empty() => {
            Some(payload[0] & LED_INDICATOR_MASK)
        }
        _ => None,
    }
}

/// Keyboard LED bitmask from the payload of a SET_REPORT control request, where the
/// report identifier travels in the request value, not in the payload. `None` for
/// any other report identifier.
pub fn led_indicators_from_control_payload(report_identifier: u8, payload: &[u8]) -> Option<u8> {
    if report_identifier == REPORT_IDENTIFIER_KEYBOARD && !payload.is_empty() {
        Some(payload[0] & LED_INDICATOR_MASK)
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn queue_pushes_and_pops_in_order() {
        let mut queue = ReportQueue::<4>::new();
        assert_eq!(queue.pop(), None);
        queue.push(REPORT_IDENTIFIER_KEYBOARD, &[1, 2, 3]).unwrap();
        queue.push(REPORT_IDENTIFIER_SYSTEM, &[0x81]).unwrap();
        assert_eq!(
            queue.pop(),
            Some((REPORT_IDENTIFIER_KEYBOARD, [1, 2, 3, 0, 0, 0, 0, 0], 3))
        );
        assert_eq!(
            queue.pop(),
            Some((REPORT_IDENTIFIER_SYSTEM, [0x81, 0, 0, 0, 0, 0, 0, 0], 1))
        );
        assert_eq!(queue.pop(), None);
    }

    #[test]
    fn queue_wraps_around() {
        let mut queue = ReportQueue::<3>::new();
        for round in 0..10u8 {
            for index in 0..3u8 {
                queue
                    .push(REPORT_IDENTIFIER_CONSUMER, &[round, index])
                    .unwrap();
            }
            for index in 0..3u8 {
                let (identifier, bytes, length) = queue.pop().unwrap();
                assert_eq!(identifier, REPORT_IDENTIFIER_CONSUMER);
                assert_eq!(&bytes[..length], &[round, index]);
            }
            assert_eq!(queue.pop(), None);
        }
    }

    #[test]
    fn full_queue_refuses_the_report_with_busy() {
        let mut queue = ReportQueue::<2>::new();
        queue.push(REPORT_IDENTIFIER_MOUSE, &[1]).unwrap();
        queue.push(REPORT_IDENTIFIER_MOUSE, &[2]).unwrap();
        // The report is not accepted, not dropped silently.
        assert_eq!(queue.push(REPORT_IDENTIFIER_MOUSE, &[3]), Err(Error::Busy));
        // After a pop, a push fits again.
        assert!(queue.pop().is_some());
        queue.push(REPORT_IDENTIFIER_MOUSE, &[3]).unwrap();
        assert_eq!(queue.pop().unwrap().2, 1);
        assert_eq!(queue.pop().unwrap().2, 1);
        assert_eq!(queue.pop(), None);
    }

    #[test]
    fn the_default_queue_capacity_holds_reports_until_the_runner_pops() {
        let mut queue = ReportQueue::<REPORT_QUEUE_CAPACITY>::new();
        for index in 0..REPORT_QUEUE_CAPACITY as u8 {
            queue.push(REPORT_IDENTIFIER_MOUSE, &[index]).unwrap();
        }
        assert_eq!(queue.push(REPORT_IDENTIFIER_MOUSE, &[0]), Err(Error::Busy));
        for index in 0..REPORT_QUEUE_CAPACITY as u8 {
            let (_, bytes, length) = queue.pop().unwrap();
            assert_eq!(&bytes[..length], &[index]);
        }
        assert_eq!(queue.pop(), None);
    }

    #[test]
    fn queue_rejects_an_unknown_identifier_or_a_long_report() {
        let mut queue = ReportQueue::<2>::new();
        assert_eq!(queue.push(0, &[0]), Err(Error::Other));
        assert_eq!(queue.push(5, &[0]), Err(Error::Other));
        assert_eq!(
            queue.push(REPORT_IDENTIFIER_KEYBOARD, &[0; REPORT_MAX_BYTES + 1]),
            Err(Error::Other)
        );
        assert_eq!(queue.pop(), None);
        // The largest report fits.
        queue
            .push(REPORT_IDENTIFIER_KEYBOARD, &[0xFF; REPORT_MAX_BYTES])
            .unwrap();
    }

    #[test]
    fn framing_prefixes_the_identifier_byte() {
        let (frame, length) = frame_report(
            REPORT_IDENTIFIER_KEYBOARD,
            &[0x02, 0, 0x04, 0x28, 0, 0, 0, 0],
        )
        .unwrap();
        assert_eq!(&frame[..length], &[1, 0x02, 0, 0x04, 0x28, 0, 0, 0, 0]);

        let (frame, length) =
            frame_report(REPORT_IDENTIFIER_CONSUMER, &0x00CDu16.to_le_bytes()).unwrap();
        assert_eq!(&frame[..length], &[2, 0xCD, 0x00]);

        let (frame, length) =
            frame_report(REPORT_IDENTIFIER_MOUSE, &[1, 0xFF, 2, 3, 0xFC]).unwrap();
        assert_eq!(&frame[..length], &[3, 1, 0xFF, 2, 3, 0xFC]);

        let (frame, length) = frame_report(REPORT_IDENTIFIER_SYSTEM, &[0x82]).unwrap();
        assert_eq!(&frame[..length], &[4, 0x82]);
    }

    #[test]
    fn framing_rejects_an_unknown_identifier_or_a_long_report() {
        assert!(frame_report(0, &[0]).is_none());
        assert!(frame_report(9, &[0]).is_none());
        assert!(frame_report(REPORT_IDENTIFIER_SYSTEM, &[0; REPORT_MAX_BYTES + 1]).is_none());
        assert!(frame_report(REPORT_IDENTIFIER_SYSTEM, &[0; REPORT_MAX_BYTES]).is_some());
    }

    #[test]
    fn led_report_on_the_out_endpoint_carries_the_identifier_first() {
        // Num lock and scroll lock on.
        assert_eq!(
            led_indicators_from_output_report(&[REPORT_IDENTIFIER_KEYBOARD, 0b101]),
            Some(0b101)
        );
        // Only the three contract bits are kept; the rest of the LED byte is padding
        // and the kana and compose LEDs.
        assert_eq!(
            led_indicators_from_output_report(&[REPORT_IDENTIFIER_KEYBOARD, 0xFF]),
            Some(LED_INDICATOR_MASK)
        );
        // Another report identifier, or an empty payload, carries no LED state.
        assert_eq!(
            led_indicators_from_output_report(&[REPORT_IDENTIFIER_MOUSE, 0xFF]),
            None
        );
        assert_eq!(
            led_indicators_from_output_report(&[REPORT_IDENTIFIER_KEYBOARD]),
            None
        );
        assert_eq!(led_indicators_from_output_report(&[]), None);
    }

    #[test]
    fn led_control_payload_carries_no_identifier_byte() {
        assert_eq!(
            led_indicators_from_control_payload(REPORT_IDENTIFIER_KEYBOARD, &[0b010]),
            Some(0b010)
        );
        assert_eq!(
            led_indicators_from_control_payload(REPORT_IDENTIFIER_KEYBOARD, &[0xFF]),
            Some(LED_INDICATOR_MASK)
        );
        assert_eq!(
            led_indicators_from_control_payload(REPORT_IDENTIFIER_SYSTEM, &[0xFF]),
            None
        );
        assert_eq!(
            led_indicators_from_control_payload(REPORT_IDENTIFIER_KEYBOARD, &[]),
            None
        );
    }

    #[test]
    fn only_the_four_report_identifiers_are_known() {
        for identifier in [
            REPORT_IDENTIFIER_KEYBOARD,
            REPORT_IDENTIFIER_CONSUMER,
            REPORT_IDENTIFIER_MOUSE,
            REPORT_IDENTIFIER_SYSTEM,
        ] {
            assert!(is_known_identifier(identifier));
        }
        for identifier in [0u8, 5, 42, u8::MAX] {
            assert!(!is_known_identifier(identifier));
        }
    }
}
