#![cfg(feature = "input-queue")]
//! A full input queue reports how many events were dropped and never the
//! events themselves, which carry typed or pasted characters.

extern crate alloc;

use alloc::{fmt, string::String, vec::Vec};
use drivers_shared::devices::{dropped_input_events, send_input_event};
use log::{Level, LoggerTrait};
use std::sync::Mutex;
use synchronization::{blocking_mutex::raw::CriticalSectionRawMutex, channel::Channel};

static MESSAGES: Mutex<Vec<String>> = Mutex::new(Vec::new());

struct CaptureLogger;

impl LoggerTrait for CaptureLogger {
    fn enabled(&self, _: Level) -> bool {
        true
    }

    fn write(&self, arguments: fmt::Arguments) {
        MESSAGES.lock().unwrap().push(alloc::format!("{arguments}"));
    }
}

/// Stand-in for a key event: its `Debug` output shows the typed character.
#[derive(Debug, Clone, Copy)]
struct KeyEvent {
    character: char,
}

#[test]
fn dropped_events_are_logged_without_content() {
    log::initialize(&CaptureLogger).unwrap();

    let channel: Channel<CriticalSectionRawMutex, KeyEvent, 2> = Channel::new();
    let sender = channel.sender();

    // A distinctive character, as a typed or pasted password would contain.
    let secret = KeyEvent { character: 'Q' };
    assert_eq!(secret.character, 'Q');

    assert!(send_input_event(&sender, secret));
    assert!(send_input_event(&sender, secret));
    assert_eq!(dropped_input_events(), 0);
    assert!(MESSAGES.lock().unwrap().is_empty());

    // Totals 1, 2 and 4 are reported; 3 is not.
    for _ in 0..4 {
        assert!(!send_input_event(&sender, secret));
    }
    assert_eq!(dropped_input_events(), 4);

    let messages = MESSAGES.lock().unwrap();
    assert_eq!(messages.len(), 3);
    for (message, total) in messages.iter().zip([1, 2, 4]) {
        assert!(message.contains(&alloc::format!("{total} events dropped")));
        assert!(!message.contains("KeyEvent"), "{message}");
        assert!(!message.contains('Q'), "{message}");
    }
}
