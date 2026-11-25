//! Transport-independent logic of the USB human interface lower half: the bounded
//! report queue between `HumanInterfaceController::send_report` and the USB runner
//! task, the report framing (the identifier byte in front of the report bytes), the
//! parsing of the keyboard LED output report, the connection state machine driven by
//! the USB bus events, and the endpoint buffer sizes.
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

/// Poll interval of the HID endpoints, in milliseconds.
pub const HID_POLL_MILLISECONDS: u8 = 1;
/// Max packet size of the HID and CDC endpoints, in bytes (full speed).
pub const ENDPOINT_MAX_PACKET_SIZE: usize = 64;
/// Buffer for received OUT packets. Every OUT endpoint takes its max packet size
/// from it, including the control endpoint 0 OUT that `Driver::start` allocates:
/// EP0 control OUT, CDC bulk OUT and HID interrupt OUT, so three times 64 bytes.
pub const ENDPOINT_OUT_BUFFER_SIZE: usize = 3 * ENDPOINT_MAX_PACKET_SIZE;
/// Largest control request of the composite device: the HID report descriptor read.
pub const CONTROL_BUFFER_SIZE: usize = 256;

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

/// Length of the input report of `identifier` in bytes, as
/// [`peripherals::human_interface::REPORT_DESCRIPTOR`] declares it: the keyboard
/// report is 8 bytes, the consumer report 2, the mouse report 5 and the system
/// report 1. `None` for an unknown identifier.
pub const fn report_length(identifier: u8) -> Option<usize> {
    match identifier {
        REPORT_IDENTIFIER_KEYBOARD => Some(8),
        REPORT_IDENTIFIER_CONSUMER => Some(2),
        REPORT_IDENTIFIER_MOUSE => Some(5),
        REPORT_IDENTIFIER_SYSTEM => Some(1),
        _ => None,
    }
}

/// The all-zero report of `identifier`, which releases every key, button and
/// control of that function. Sent when the host comes back, so that no key stays
/// held after a re-enumeration.
pub const fn release_report(identifier: u8) -> Option<([u8; REPORT_MAX_BYTES], usize)> {
    match report_length(identifier) {
        Some(length) => Some(([0; REPORT_MAX_BYTES], length)),
        None => None,
    }
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

    /// Drop every queued report, on a bus event that invalidates them.
    pub fn clear(&mut self) {
        self.head = 0;
        self.count = 0;
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
/// report identifier travels in the request value. Hosts send both layouts: with
/// report identifiers in the descriptor, some send the identifier as the first data
/// byte (a two-byte payload starting with the keyboard identifier), others send the
/// report itself (a one-byte payload). `None` for any other report identifier or
/// payload length.
pub fn led_indicators_from_control_payload(report_identifier: u8, payload: &[u8]) -> Option<u8> {
    if report_identifier != REPORT_IDENTIFIER_KEYBOARD {
        return None;
    }
    match payload {
        [REPORT_IDENTIFIER_KEYBOARD, leds] => Some(*leds & LED_INDICATOR_MASK),
        [leds] => Some(*leds & LED_INDICATOR_MASK),
        _ => None,
    }
}

/// Direction of a GET_REPORT control request.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReportDirection {
    /// An input report, read by the host.
    In,
    /// An output report, written by the host.
    Out,
    /// A feature report.
    Feature,
}

/// Builds the GET_REPORT response for a report into `buffer`: the report
/// identifier byte first, then the report bytes, because the descriptor uses
/// numbered reports. The keyboard output report answers the current LED byte;
/// an input report is answered with zeros of its declared length. Returns the
/// response length, or `None` for an unknown report, an unsupported direction or
/// a buffer that is too small.
pub fn get_report_response(
    direction: ReportDirection,
    identifier: u8,
    led_indicators: u8,
    buffer: &mut [u8],
) -> Option<usize> {
    let report_bytes = match direction {
        ReportDirection::Out if identifier == REPORT_IDENTIFIER_KEYBOARD => 1,
        ReportDirection::In => report_length(identifier)?,
        _ => return None,
    };
    let length = 1 + report_bytes;
    if buffer.len() < length {
        return None;
    }
    buffer[0] = identifier;
    match direction {
        ReportDirection::Out => buffer[1] = led_indicators,
        _ => buffer[1..length].fill(0),
    }
    Some(length)
}

/// A USB bus event, as the `embassy_usb::Handler` callbacks report them.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BusEvent {
    /// Bus reset: the device is back in the Default state.
    Reset,
    /// The host set a configuration (SET_CONFIGURATION with a value).
    Configured,
    /// The host cleared the configuration (SET_CONFIGURATION with zero).
    Unconfigured,
    /// The bus entered the suspend state, which also covers an unplugged cable.
    Suspended,
    /// The bus left the suspend state.
    Resumed,
    /// The power was removed from the bus.
    PowerRemoved,
}

/// What a bus event means for the emulation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct EventOutcome {
    /// Whether a computer is connected and listening now.
    pub connected: bool,
    /// Queued and in-flight reports are stale: drop them.
    pub invalidate: bool,
    /// The host came back: send the release reports, so no key stays held.
    pub host_returned: bool,
    /// The LED state is stale too: clear it. Only on a reset, an
    /// unconfiguration, a suspend or a power removal, not on a resume: the host
    /// is still there and its LED state is still valid.
    pub clear_indicators: bool,
}

/// Connection state of the USB device, driven by the bus events. `is_connected` is
/// the configured state without suspend: a host that configured the device and is
/// not suspending the bus is connected and listening.
pub struct ConnectionState {
    configured: bool,
    suspended: bool,
    /// Bumped on every bus event, so that a report popped before the event is not
    /// delivered after it.
    generation: u32,
}

impl ConnectionState {
    /// The state after power-up: not configured, not suspended.
    pub const fn new() -> Self {
        Self {
            configured: false,
            suspended: false,
            generation: 0,
        }
    }

    /// Whether a computer is connected and listening: configured and not suspended.
    pub const fn is_connected(&self) -> bool {
        self.configured && !self.suspended
    }

    /// The current generation, to compare against the one captured before an
    /// asynchronous operation.
    pub const fn generation(&self) -> u32 {
        self.generation
    }

    /// Whether a bus event happened since `generation` was captured: the
    /// asynchronous operation that captured it must not complete with its result.
    pub const fn is_stale(&self, generation: u32) -> bool {
        self.generation != generation
    }

    /// Apply a bus event and return what it means for the emulation. Every event
    /// bumps the generation.
    pub fn apply(&mut self, event: BusEvent) -> EventOutcome {
        self.generation = self.generation.wrapping_add(1);
        match event {
            BusEvent::Reset => {
                self.configured = false;
                self.suspended = false;
                EventOutcome {
                    connected: false,
                    invalidate: true,
                    host_returned: false,
                    clear_indicators: true,
                }
            }
            BusEvent::Configured => {
                self.configured = true;
                self.suspended = false;
                EventOutcome {
                    connected: true,
                    invalidate: true,
                    host_returned: true,
                    clear_indicators: false,
                }
            }
            BusEvent::Unconfigured => {
                self.configured = false;
                EventOutcome {
                    connected: false,
                    invalidate: true,
                    host_returned: false,
                    clear_indicators: true,
                }
            }
            BusEvent::Suspended => {
                self.suspended = true;
                EventOutcome {
                    connected: false,
                    invalidate: true,
                    host_returned: false,
                    clear_indicators: true,
                }
            }
            BusEvent::Resumed => {
                self.suspended = false;
                EventOutcome {
                    connected: self.configured,
                    invalidate: true,
                    host_returned: self.configured,
                    clear_indicators: false,
                }
            }
            BusEvent::PowerRemoved => {
                self.configured = false;
                self.suspended = false;
                EventOutcome {
                    connected: false,
                    invalidate: true,
                    host_returned: false,
                    clear_indicators: true,
                }
            }
        }
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
    fn queue_clear_drops_every_queued_report() {
        let mut queue = ReportQueue::<4>::new();
        queue.push(REPORT_IDENTIFIER_MOUSE, &[1]).unwrap();
        queue.push(REPORT_IDENTIFIER_MOUSE, &[2]).unwrap();
        queue.clear();
        assert_eq!(queue.pop(), None);
        // The queue is usable again after a clear.
        queue.push(REPORT_IDENTIFIER_MOUSE, &[3]).unwrap();
        assert_eq!(queue.pop().unwrap().2, 1);
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
    fn led_control_payload_accepts_both_set_report_layouts() {
        // The identifier echoed as the first data byte, then the LED byte.
        assert_eq!(
            led_indicators_from_control_payload(
                REPORT_IDENTIFIER_KEYBOARD,
                &[REPORT_IDENTIFIER_KEYBOARD, 0b010]
            ),
            Some(0b010)
        );
        assert_eq!(
            led_indicators_from_control_payload(
                REPORT_IDENTIFIER_KEYBOARD,
                &[REPORT_IDENTIFIER_KEYBOARD, 0xFF]
            ),
            Some(LED_INDICATOR_MASK)
        );
        // The report itself, without the identifier byte.
        assert_eq!(
            led_indicators_from_control_payload(REPORT_IDENTIFIER_KEYBOARD, &[0b101]),
            Some(0b101)
        );
        assert_eq!(
            led_indicators_from_control_payload(REPORT_IDENTIFIER_KEYBOARD, &[0xFF]),
            Some(LED_INDICATOR_MASK)
        );
        // Another report identifier, or another payload length, carries no LED state.
        assert_eq!(
            led_indicators_from_control_payload(REPORT_IDENTIFIER_SYSTEM, &[0xFF]),
            None
        );
        assert_eq!(
            led_indicators_from_control_payload(REPORT_IDENTIFIER_MOUSE, &[1, 0xFF]),
            None
        );
        assert_eq!(
            led_indicators_from_control_payload(REPORT_IDENTIFIER_KEYBOARD, &[]),
            None
        );
        assert_eq!(
            led_indicators_from_control_payload(REPORT_IDENTIFIER_KEYBOARD, &[1, 2, 3]),
            None
        );
        assert_eq!(
            led_indicators_from_control_payload(REPORT_IDENTIFIER_KEYBOARD, &[2, 0xFF]),
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

    #[test]
    fn report_lengths_match_the_descriptor() {
        assert_eq!(report_length(REPORT_IDENTIFIER_KEYBOARD), Some(8));
        assert_eq!(report_length(REPORT_IDENTIFIER_CONSUMER), Some(2));
        assert_eq!(report_length(REPORT_IDENTIFIER_MOUSE), Some(5));
        assert_eq!(report_length(REPORT_IDENTIFIER_SYSTEM), Some(1));
        assert_eq!(report_length(0), None);
        assert_eq!(report_length(9), None);
    }

    #[test]
    fn release_reports_are_all_zero_of_the_identifier_length() {
        for identifier in [
            REPORT_IDENTIFIER_KEYBOARD,
            REPORT_IDENTIFIER_CONSUMER,
            REPORT_IDENTIFIER_MOUSE,
            REPORT_IDENTIFIER_SYSTEM,
        ] {
            let (bytes, length) = release_report(identifier).unwrap();
            assert_eq!(length, report_length(identifier).unwrap());
            assert!(bytes[..length].iter().all(|&byte| byte == 0));
        }
        assert!(release_report(0).is_none());
    }

    #[test]
    fn connection_starts_disconnected() {
        let state = ConnectionState::new();
        assert!(!state.is_connected());
        assert_eq!(state.generation(), 0);
    }

    #[test]
    fn configuration_connects_and_returns_the_host() {
        let mut state = ConnectionState::new();
        let outcome = state.apply(BusEvent::Configured);
        assert_eq!(
            outcome,
            EventOutcome {
                connected: true,
                invalidate: true,
                host_returned: true,
                clear_indicators: false
            }
        );
        assert!(state.is_connected());
        assert_eq!(state.generation(), 1);
    }

    #[test]
    fn suspend_disconnects_and_resume_reconnects_when_configured() {
        let mut state = ConnectionState::new();
        state.apply(BusEvent::Configured);
        let outcome = state.apply(BusEvent::Suspended);
        assert_eq!(
            outcome,
            EventOutcome {
                connected: false,
                invalidate: true,
                host_returned: false,
                clear_indicators: true
            }
        );
        assert!(!state.is_connected());
        // A resume keeps the LED state: the host is still there.
        let outcome = state.apply(BusEvent::Resumed);
        assert_eq!(
            outcome,
            EventOutcome {
                connected: true,
                invalidate: true,
                host_returned: true,
                clear_indicators: false
            }
        );
        assert!(state.is_connected());
    }

    #[test]
    fn resume_without_configuration_stays_disconnected() {
        let mut state = ConnectionState::new();
        let outcome = state.apply(BusEvent::Resumed);
        assert_eq!(
            outcome,
            EventOutcome {
                connected: false,
                invalidate: true,
                host_returned: false,
                clear_indicators: false
            }
        );
        assert!(!state.is_connected());
    }

    #[test]
    fn reset_and_unconfigure_and_power_removal_disconnect_and_invalidate() {
        for event in [
            BusEvent::Reset,
            BusEvent::Unconfigured,
            BusEvent::PowerRemoved,
        ] {
            let mut state = ConnectionState::new();
            state.apply(BusEvent::Configured);
            let generation_before = state.generation();
            let outcome = state.apply(event);
            assert_eq!(
                outcome,
                EventOutcome {
                    connected: false,
                    invalidate: true,
                    host_returned: false,
                    clear_indicators: true
                }
            );
            assert!(!state.is_connected());
            assert!(state.generation() > generation_before);
        }
    }

    #[test]
    fn every_event_bumps_the_generation() {
        let mut state = ConnectionState::new();
        for event in [
            BusEvent::Reset,
            BusEvent::Configured,
            BusEvent::Unconfigured,
            BusEvent::Suspended,
            BusEvent::Resumed,
            BusEvent::PowerRemoved,
        ] {
            let before = state.generation();
            state.apply(event);
            assert_eq!(state.generation(), before.wrapping_add(1));
        }
    }

    #[test]
    fn a_reset_after_configuration_needs_a_new_configuration() {
        let mut state = ConnectionState::new();
        state.apply(BusEvent::Configured);
        state.apply(BusEvent::Reset);
        assert!(!state.is_connected());
        // After a bus reset the host configures the device again.
        let outcome = state.apply(BusEvent::Configured);
        assert!(outcome.host_returned);
        assert!(state.is_connected());
    }

    #[test]
    fn the_out_buffer_holds_the_control_endpoint_too() {
        // EP0 control OUT, CDC bulk OUT and HID interrupt OUT each take one max
        // packet size from the OUT buffer.
        assert_eq!(ENDPOINT_OUT_BUFFER_SIZE, 3 * ENDPOINT_MAX_PACKET_SIZE);
        assert_eq!(ENDPOINT_MAX_PACKET_SIZE, 64);
    }

    #[test]
    fn is_stale_only_after_a_bus_event() {
        let mut state = ConnectionState::new();
        let generation = state.generation();
        assert!(!state.is_stale(generation));
        state.apply(BusEvent::Suspended);
        assert!(state.is_stale(generation));
        assert!(!state.is_stale(state.generation()));
    }

    #[test]
    fn get_report_answers_the_keyboard_output_report_with_the_led_byte() {
        let mut buffer = [0u8; 8];
        let length = get_report_response(
            ReportDirection::Out,
            REPORT_IDENTIFIER_KEYBOARD,
            0b101,
            &mut buffer,
        )
        .unwrap();
        assert_eq!(length, 2);
        assert_eq!(&buffer[..length], &[REPORT_IDENTIFIER_KEYBOARD, 0b101]);
        // Another output report identifier is not answered.
        assert!(
            get_report_response(
                ReportDirection::Out,
                REPORT_IDENTIFIER_MOUSE,
                0,
                &mut buffer
            )
            .is_none()
        );
    }

    #[test]
    fn get_report_answers_input_reports_with_the_identifier_and_zeros() {
        let mut buffer = [0u8; 16];
        for (identifier, report_bytes) in [
            (REPORT_IDENTIFIER_KEYBOARD, 8),
            (REPORT_IDENTIFIER_CONSUMER, 2),
            (REPORT_IDENTIFIER_MOUSE, 5),
            (REPORT_IDENTIFIER_SYSTEM, 1),
        ] {
            let length =
                get_report_response(ReportDirection::In, identifier, 0xFF, &mut buffer).unwrap();
            assert_eq!(length, 1 + report_bytes);
            assert_eq!(buffer[0], identifier);
            assert!(buffer[1..length].iter().all(|&byte| byte == 0));
        }
        // An unknown input report identifier, a feature report or a small buffer
        // is not answered.
        assert!(get_report_response(ReportDirection::In, 0, 0, &mut buffer).is_none());
        assert!(
            get_report_response(
                ReportDirection::Feature,
                REPORT_IDENTIFIER_KEYBOARD,
                0,
                &mut buffer
            )
            .is_none()
        );
        assert!(
            get_report_response(
                ReportDirection::In,
                REPORT_IDENTIFIER_KEYBOARD,
                0,
                &mut buffer[..8]
            )
            .is_none()
        );
    }
}
