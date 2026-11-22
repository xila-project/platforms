use alloc::vec;
use alloc::vec::Vec;
use core::sync::atomic::{AtomicUsize, Ordering};
use core::task::{Context, Poll, Waker};
use file_system::{
    ControlCommand, ControlCommandIdentifier, DirectBaseOperations, DirectCharacterDevice,
    IoErrorKind, MountOperations, Result, Size,
    character_device::{
        GET_TERMINAL_ATTRIBUTES, SET_TERMINAL_ATTRIBUTES, TerminalAttributes, TerminalFlags,
    },
};
use shared::AnyByLayout;

use super::device::TerminalInputDevice;
use super::logic::{EchoBuffer, EchoSink, LineEditor};
use super::output::TerminalOutputDevice;

// -----------------------------------------------------------------------------
// Layer 1 Unit Tests: LineEditor
// -----------------------------------------------------------------------------

#[test]
fn plain_line_cr() {
    let mut editor = LineEditor::<256>::new();
    let mut echo = Vec::new();
    let res = editor.feed_slice(b"hello\r", &mut echo);
    assert!(res.line_ready);
    assert!(!res.eof);
    assert_eq!(echo, b"hello\r\n");

    let mut buf = [0u8; 32];
    let n = editor.pop_completed(&mut buf);
    assert_eq!(&buf[..n], b"hello\n");
    assert!(!editor.has_completed_bytes());
}

#[test]
fn plain_line_lf() {
    let mut editor = LineEditor::<256>::new();
    let mut echo = Vec::new();
    let res = editor.feed_slice(b"world\n", &mut echo);
    assert!(res.line_ready);
    assert!(!res.eof);
    assert_eq!(echo, b"world\r\n");

    let mut buf = [0u8; 32];
    let n = editor.pop_completed(&mut buf);
    assert_eq!(&buf[..n], b"world\n");
}

#[test]
fn plain_line_crlf_together() {
    let mut editor = LineEditor::<256>::new();
    let mut echo = Vec::new();
    let res = editor.feed_slice(b"hello\r\n", &mut echo);
    assert!(res.line_ready);
    assert_eq!(echo, b"hello\r\n");

    let mut buf = [0u8; 32];
    let n = editor.pop_completed(&mut buf);
    assert_eq!(&buf[..n], b"hello\n");
    assert!(!editor.has_completed_bytes());
}

#[test]
fn plain_line_crlf_split_across_calls() {
    let mut editor = LineEditor::<256>::new();
    let mut echo = Vec::new();

    let res1 = editor.feed_slice(b"hello\r", &mut echo);
    assert!(res1.line_ready);
    assert_eq!(echo, b"hello\r\n");

    echo.clear();
    let res2 = editor.feed_slice(b"\n", &mut echo);
    assert!(!res2.line_ready);
    assert!(echo.is_empty()); // \n was swallowed!

    let mut buf = [0u8; 32];
    let n = editor.pop_completed(&mut buf);
    assert_eq!(&buf[..n], b"hello\n");
    assert!(!editor.has_completed_bytes());
}

#[test]
fn consecutive_cr_cr_produces_two_line_ends() {
    let mut editor = LineEditor::<256>::new();
    let mut echo = Vec::new();

    editor.feed_slice(b"one\r\r", &mut echo);
    assert_eq!(echo, b"one\r\n\r\n");

    let mut buf = [0u8; 32];
    let n = editor.pop_completed(&mut buf);
    assert_eq!(&buf[..n], b"one\n\n");
}

#[test]
fn backspace_ascii() {
    let mut editor = LineEditor::<256>::new();
    let mut echo = Vec::new();

    editor.feed_slice(b"ab\x08c\n", &mut echo);
    assert_eq!(echo, b"ab\x08 \x08c\r\n");

    let mut buf = [0u8; 32];
    let n = editor.pop_completed(&mut buf);
    assert_eq!(&buf[..n], b"ac\n");
}

#[test]
fn backspace_del_ascii() {
    let mut editor = LineEditor::<256>::new();
    let mut echo = Vec::new();

    editor.feed_slice(b"xy\x7Fz\n", &mut echo);
    assert_eq!(echo, b"xy\x08 \x08z\r\n");

    let mut buf = [0u8; 32];
    let n = editor.pop_completed(&mut buf);
    assert_eq!(&buf[..n], b"xz\n");
}

#[test]
fn backspace_utf8_2byte() {
    let mut editor = LineEditor::<256>::new();
    let mut echo = Vec::new();

    // 'é' in UTF-8 is [0xC3, 0xA9]
    let mut input = Vec::new();
    input.extend_from_slice(b"a");
    input.extend_from_slice("é".as_bytes());
    input.push(0x08); // backspace
    input.extend_from_slice(b"b\n");

    editor.feed_slice(&input, &mut echo);
    // Echo: "a" + "é" + "\b \b" + "b\r\n"
    let mut expected_echo = Vec::new();
    expected_echo.extend_from_slice(b"a");
    expected_echo.extend_from_slice("é".as_bytes());
    expected_echo.extend_from_slice(b"\x08 \x08b\r\n");
    assert_eq!(echo, expected_echo);

    let mut buf = [0u8; 32];
    let n = editor.pop_completed(&mut buf);
    assert_eq!(&buf[..n], b"ab\n");
}

#[test]
fn backspace_utf8_3byte() {
    let mut editor = LineEditor::<256>::new();
    let mut echo = Vec::new();

    // '€' in UTF-8 is [0xE2, 0x82, 0xAC]
    let mut input = Vec::new();
    input.extend_from_slice(b"a");
    input.extend_from_slice("€".as_bytes());
    input.push(0x08); // backspace
    input.extend_from_slice(b"b\n");

    editor.feed_slice(&input, &mut echo);
    let mut expected_echo = Vec::new();
    expected_echo.extend_from_slice(b"a");
    expected_echo.extend_from_slice("€".as_bytes());
    expected_echo.extend_from_slice(b"\x08 \x08b\r\n");
    assert_eq!(echo, expected_echo);

    let mut buf = [0u8; 32];
    let n = editor.pop_completed(&mut buf);
    assert_eq!(&buf[..n], b"ab\n");
}

#[test]
fn backspace_utf8_4byte() {
    let mut editor = LineEditor::<256>::new();
    let mut echo = Vec::new();

    // '🦀' in UTF-8 is [0xF0, 0x9F, 0xA6, 0x80]
    let mut input = Vec::new();
    input.extend_from_slice(b"a");
    input.extend_from_slice("🦀".as_bytes());
    input.push(0x08); // backspace
    input.extend_from_slice(b"b\n");

    editor.feed_slice(&input, &mut echo);
    let mut expected_echo = Vec::new();
    expected_echo.extend_from_slice(b"a");
    expected_echo.extend_from_slice("🦀".as_bytes());
    expected_echo.extend_from_slice(b"\x08 \x08b\r\n");
    assert_eq!(echo, expected_echo);

    let mut buf = [0u8; 32];
    let n = editor.pop_completed(&mut buf);
    assert_eq!(&buf[..n], b"ab\n");
}

#[test]
fn backspace_empty_line() {
    let mut editor = LineEditor::<256>::new();
    let mut echo = Vec::new();

    editor.feed_byte(0x08, &mut echo);
    editor.feed_byte(0x7F, &mut echo);
    assert!(echo.is_empty());
    assert_eq!(editor.current_line(), b"");
}

#[test]
fn ctrl_u_erases_line() {
    let mut editor = LineEditor::<256>::new();
    let mut echo = Vec::new();

    editor.feed_slice(b"hello", &mut echo);
    echo.clear();
    editor.feed_byte(0x15, &mut echo); // Ctrl-U
    assert_eq!(echo, b"\x08 \x08\x08 \x08\x08 \x08\x08 \x08\x08 \x08"); // 5 characters
    assert_eq!(editor.current_line(), b"");

    echo.clear();
    editor.feed_slice(b"ok\n", &mut echo);
    let mut buf = [0u8; 32];
    let n = editor.pop_completed(&mut buf);
    assert_eq!(&buf[..n], b"ok\n");
}

#[test]
fn ctrl_u_utf8() {
    let mut editor = LineEditor::<256>::new();
    let mut echo = Vec::new();

    // "aé" -> 1 ASCII + 1 2-byte UTF-8 = 2 characters total
    let mut input = Vec::new();
    input.extend_from_slice("aé".as_bytes());
    editor.feed_slice(&input, &mut echo);
    echo.clear();

    editor.feed_byte(0x15, &mut echo); // Ctrl-U
    assert_eq!(echo, b"\x08 \x08\x08 \x08"); // Exactly 2 characters erased
    assert_eq!(editor.current_line(), b"");
}

#[test]
fn ctrl_u_empty_line() {
    let mut editor = LineEditor::<256>::new();
    let mut echo = Vec::new();

    editor.feed_byte(0x15, &mut echo);
    assert!(echo.is_empty());
    assert_eq!(editor.current_line(), b"");
}

#[test]
fn overflow_at_capacity() {
    // Capacity 10 bytes
    let mut editor = LineEditor::<10>::new();
    let mut echo = Vec::new();

    // Feed 10 bytes: should all be accepted
    editor.feed_slice(b"0123456789", &mut echo);
    assert_eq!(echo, b"0123456789");

    // 11th byte: overflow, bell 0x07 echoed, character discarded
    echo.clear();
    editor.feed_byte(b'a', &mut echo);
    assert_eq!(echo, b"\x07");
    assert_eq!(editor.current_line(), b"0123456789");

    // Line still terminates correctly on Enter
    echo.clear();
    let res = editor.feed_byte(b'\n', &mut echo);
    assert!(res.line_ready);
    assert_eq!(echo, b"\r\n");

    let mut buf = [0u8; 32];
    let n = editor.pop_completed(&mut buf);
    assert_eq!(&buf[..n], b"0123456789\n");
}

#[test]
fn overflow_utf8_multibyte_not_split() {
    // Capacity 4 bytes. We enter "ab" (2 bytes).
    // Then attempt to enter '€' (3 bytes: [0xE2, 0x82, 0xAC]).
    // 2 + 3 = 5 > 4: cannot fit!
    let mut editor = LineEditor::<4>::new();
    let mut echo = Vec::new();

    editor.feed_slice(b"ab", &mut echo);
    echo.clear();

    editor.feed_slice("€".as_bytes(), &mut echo);
    // Bell emitted for the discarded character, continuation bytes swallowed
    assert_eq!(echo, b"\x07");
    assert_eq!(editor.current_line(), b"ab"); // Not split in middle!

    // Now terminate
    echo.clear();
    editor.feed_byte(b'\n', &mut echo);
    let mut buf = [0u8; 32];
    let n = editor.pop_completed(&mut buf);
    assert_eq!(&buf[..n], b"ab\n");
}

#[test]
fn ctrl_c_discards_and_makes_empty_line() {
    let mut editor = LineEditor::<256>::new();
    let mut echo = Vec::new();

    editor.feed_slice(b"partial command", &mut echo);
    echo.clear();

    let res = editor.feed_byte(0x03, &mut echo); // Ctrl-C
    assert!(res.line_ready);
    assert_eq!(echo, b"^C\r\n");
    assert_eq!(editor.current_line(), b"");

    let mut buf = [0u8; 32];
    let n = editor.pop_completed(&mut buf);
    assert_eq!(&buf[..n], b"\n"); // Empty line delivered
}

#[test]
fn ctrl_d_empty_and_non_empty_lines() {
    let mut editor = LineEditor::<256>::new();
    let mut echo = Vec::new();

    // 1. Non-empty line: Ctrl-D ignored
    editor.feed_slice(b"abc", &mut echo);
    let res = editor.feed_byte(0x04, &mut echo);
    assert!(!res.eof);
    assert!(!res.line_ready);
    assert_eq!(editor.current_line(), b"abc");

    // Clear line with Ctrl-U
    editor.feed_byte(0x15, &mut echo);
    assert_eq!(editor.current_line(), b"");

    // 2. Empty line: Ctrl-D reports EOF
    let res_eof = editor.feed_byte(0x04, &mut echo);
    assert!(res_eof.eof);
    assert!(editor.has_eof());

    // Consuming EOF
    assert!(editor.take_eof());
    // Exactly once: next take is false
    assert!(!editor.take_eof());
}

#[test]
fn tab_stored_and_echoed_as_space() {
    let mut editor = LineEditor::<256>::new();
    let mut echo = Vec::new();

    editor.feed_byte(0x09, &mut echo);
    assert_eq!(echo, b" ");
    assert_eq!(editor.current_line(), b" ");
}

#[test]
fn escape_sequences_swallowed() {
    let mut editor = LineEditor::<256>::new();
    let mut echo = Vec::new();

    // "a" + UpArrow (\x1B[A) + "b" + DownArrow (\x1B[B) + "c" + SS3 Home (\x1BOH) + "d\n"
    editor.feed_slice(b"a\x1B[Ab\x1B[Bc\x1BOHd\n", &mut echo);
    assert_eq!(echo, b"abcd\r\n");

    let mut buf = [0u8; 32];
    let n = editor.pop_completed(&mut buf);
    assert_eq!(&buf[..n], b"abcd\n");
}

#[test]
fn other_control_bytes_ignored() {
    let mut editor = LineEditor::<256>::new();
    let mut echo = Vec::new();

    // Controls: 0x00 (NUL), 0x01 (Ctrl-A), 0x07 (BEL), 0x1A (Ctrl-Z)
    editor.feed_slice(b"a\x00\x01b\x07\x1Ac\n", &mut echo);
    assert_eq!(echo, b"abc\r\n");

    let mut buf = [0u8; 32];
    let n = editor.pop_completed(&mut buf);
    assert_eq!(&buf[..n], b"abc\n");
}

#[test]
fn reader_buffer_smaller_than_line() {
    let mut editor = LineEditor::<256>::new();
    let mut echo = Vec::new();

    editor.feed_slice(b"hello\n", &mut echo);

    // Read with small buffer of 2 bytes
    let mut buf = [0u8; 2];
    let n1 = editor.pop_completed(&mut buf);
    assert_eq!(n1, 2);
    assert_eq!(&buf, b"he");

    let n2 = editor.pop_completed(&mut buf);
    assert_eq!(n2, 2);
    assert_eq!(&buf, b"ll");

    let n3 = editor.pop_completed(&mut buf);
    assert_eq!(n3, 2);
    assert_eq!(&buf, b"o\n");

    let n4 = editor.pop_completed(&mut buf);
    assert_eq!(n4, 0);
}

#[test]
fn type_ahead_second_line_before_first_read() {
    let mut editor = LineEditor::<256>::new();
    let mut echo = Vec::new();

    editor.feed_slice(b"first\n", &mut echo);
    editor.feed_slice(b"second\n", &mut echo);

    let mut buf = [0u8; 32];
    let n1 = editor.pop_completed(&mut buf);
    assert_eq!(&buf[..n1], b"first\nsecond\n");
}

#[test]
fn echo_buffer_fixed_capacity() {
    let mut echo = EchoBuffer::<8>::new();
    echo.emit(b"12345");
    assert_eq!(echo.as_bytes(), b"12345");
    echo.emit(b"67890"); // Truncated at 8
    assert_eq!(echo.as_bytes(), b"12345678");
    echo.clear();
    assert!(echo.is_empty());
}

// -----------------------------------------------------------------------------
// Layer 2 Device Tests: Mocks and VFS operations
// -----------------------------------------------------------------------------

struct ScriptedState {
    chunks: Vec<Vec<u8>>,
    current_index: usize,
}

struct ScriptedInputDevice {
    state: std::sync::Mutex<ScriptedState>,
    waker_count: AtomicUsize,
}

impl ScriptedInputDevice {
    fn new(chunks: Vec<&[u8]>) -> Self {
        Self {
            state: std::sync::Mutex::new(ScriptedState {
                chunks: chunks.into_iter().map(|c| c.to_vec()).collect(),
                current_index: 0,
            }),
            waker_count: AtomicUsize::new(0),
        }
    }

    fn waker_registrations(&self) -> usize {
        self.waker_count.load(Ordering::SeqCst)
    }

    /// Make `chunk` the next raw input, after the chunks not read yet.
    fn push_chunk(&self, chunk: &[u8]) {
        self.state.lock().unwrap().chunks.push(chunk.to_vec());
    }
}

impl DirectBaseOperations for ScriptedInputDevice {
    fn read(
        &self,
        _context: &mut Context<'_>,
        buffer: &mut [u8],
        _absolute_position: Size,
    ) -> Poll<Result<usize>> {
        let mut state = self.state.lock().unwrap();
        if state.current_index < state.chunks.len() {
            let idx = state.current_index;
            state.current_index += 1;
            let chunk = &state.chunks[idx];
            let n = buffer.len().min(chunk.len());
            buffer[..n].copy_from_slice(&chunk[..n]);
            Poll::Ready(Ok(n))
        } else {
            self.waker_count.fetch_add(1, Ordering::SeqCst);
            Poll::Pending
        }
    }

    fn write(
        &self,
        _context: &mut Context<'_>,
        _buffer: &[u8],
        _absolute_position: Size,
    ) -> Poll<Result<usize>> {
        Poll::Ready(Err(IoErrorKind::Unsupported))
    }
}

impl MountOperations for ScriptedInputDevice {}
impl DirectCharacterDevice for ScriptedInputDevice {}

struct RecordingOutputDevice {
    output: std::sync::Mutex<Vec<u8>>,
}

impl RecordingOutputDevice {
    fn new() -> Self {
        Self {
            output: std::sync::Mutex::new(Vec::new()),
        }
    }

    fn recorded(&self) -> Vec<u8> {
        self.output.lock().unwrap().clone()
    }
}

impl DirectBaseOperations for RecordingOutputDevice {
    fn read(
        &self,
        _context: &mut Context<'_>,
        _buffer: &mut [u8],
        _absolute_position: Size,
    ) -> Poll<Result<usize>> {
        Poll::Ready(Err(IoErrorKind::Unsupported))
    }

    fn write(
        &self,
        _context: &mut Context<'_>,
        buffer: &[u8],
        _absolute_position: Size,
    ) -> Poll<Result<usize>> {
        self.output.lock().unwrap().extend_from_slice(buffer);
        Poll::Ready(Ok(buffer.len()))
    }
}

impl MountOperations for RecordingOutputDevice {}
impl DirectCharacterDevice for RecordingOutputDevice {}

struct StalledOutputDevice;

impl DirectBaseOperations for StalledOutputDevice {
    fn read(
        &self,
        _context: &mut Context<'_>,
        _buffer: &mut [u8],
        _absolute_position: Size,
    ) -> Poll<Result<usize>> {
        Poll::Ready(Err(IoErrorKind::Unsupported))
    }

    fn write(
        &self,
        _context: &mut Context<'_>,
        _buffer: &[u8],
        _absolute_position: Size,
    ) -> Poll<Result<usize>> {
        Poll::Pending
    }
}

impl MountOperations for StalledOutputDevice {}
impl DirectCharacterDevice for StalledOutputDevice {}

#[test]
fn device_pending_only_after_inner_input_pending() {
    let input = ScriptedInputDevice::new(vec![b"hel", b"lo"]);
    let output = RecordingOutputDevice::new();
    let terminal = TerminalInputDevice::new(input, output);

    let mut buf = [0u8; 32];
    let waker = Waker::noop();
    let mut context = Context::from_waker(waker);

    // Initial read: pulls "hel", then pulls "lo", neither completes line.
    // Script is exhausted, inner input returns Pending, registering waker.
    // TerminalInputDevice returns Pending!
    let poll_res = terminal.read(&mut context, &mut buf, 0);
    assert_eq!(poll_res, Poll::Pending);

    // Inner input was polled until Pending and registered the waker once
    assert_eq!(terminal.inner_in().waker_registrations(), 1);
}

#[test]
fn device_line_arriving_in_chunks_delivered_when_complete() {
    let input = ScriptedInputDevice::new(vec![b"ab", b"cd", b"\n"]);
    let output = RecordingOutputDevice::new();
    let terminal = TerminalInputDevice::new(input, output);

    let mut buf = [0u8; 32];
    let waker = Waker::noop();
    let mut context = Context::from_waker(waker);

    let poll_res = terminal.read(&mut context, &mut buf, 0);
    assert_eq!(poll_res, Poll::Ready(Ok(5)));
    assert_eq!(&buf[..5], b"abcd\n");
}

#[test]
fn device_echo_bytes_reach_output_in_order() {
    let input = ScriptedInputDevice::new(vec![b"a\x08b\r"]);
    let output = RecordingOutputDevice::new();
    let terminal = TerminalInputDevice::new(input, output);

    let mut buf = [0u8; 32];
    let waker = Waker::noop();
    let mut context = Context::from_waker(waker);

    let poll_res = terminal.read(&mut context, &mut buf, 0);
    assert_eq!(poll_res, Poll::Ready(Ok(2)));
    assert_eq!(&buf[..2], b"b\n");

    // Echo: "a" + "\x08 \x08" + "b" + "\r\n"
    assert_eq!(&terminal.inner_out().recorded()[..], b"a\x08 \x08b\r\n");
}

#[test]
fn device_stalled_output_does_not_prevent_delivery() {
    let input = ScriptedInputDevice::new(vec![b"command\n"]);
    let output = StalledOutputDevice;
    let terminal = TerminalInputDevice::new(input, output);

    let mut buf = [0u8; 32];
    let waker = Waker::noop();
    let mut context = Context::from_waker(waker);

    // Output write returns Pending, which is dropped per echo policy.
    // Line delivery succeeds without stalling!
    let poll_res = terminal.read(&mut context, &mut buf, 0);
    assert_eq!(poll_res, Poll::Ready(Ok(8)));
    assert_eq!(&buf[..8], b"command\n");
}

#[test]
fn device_control_forwarding() {
    struct MockTerminalDevice;
    impl DirectBaseOperations for MockTerminalDevice {
        fn read(
            &self,
            _context: &mut Context<'_>,
            _buf: &mut [u8],
            _pos: Size,
        ) -> Poll<Result<usize>> {
            Poll::Pending
        }
        fn write(
            &self,
            _context: &mut Context<'_>,
            _buf: &[u8],
            _pos: Size,
        ) -> Poll<Result<usize>> {
            Poll::Ready(Ok(0))
        }
        fn control(
            &self,
            _context: &mut Context<'_>,
            command: ControlCommandIdentifier,
            _input: &AnyByLayout,
            output: &mut AnyByLayout,
        ) -> Poll<Result<()>> {
            if command == file_system::character_device::IS_A_TERMINAL::IDENTIFIER {
                let out: &mut bool =
                    file_system::character_device::IS_A_TERMINAL::cast_output(output).unwrap();
                *out = true;
                Poll::Ready(Ok(()))
            } else {
                Poll::Ready(Err(IoErrorKind::Unsupported))
            }
        }
    }
    impl MountOperations for MockTerminalDevice {}
    impl DirectCharacterDevice for MockTerminalDevice {}

    let terminal = TerminalInputDevice::new(MockTerminalDevice, MockTerminalDevice);
    let mut is_terminal = false;
    let out = AnyByLayout::from_mutable(&mut is_terminal);
    let input = ();
    let in_layout = AnyByLayout::from(&input);
    let waker = Waker::noop();
    let mut context = Context::from_waker(waker);

    let res = terminal.control(
        &mut context,
        file_system::character_device::IS_A_TERMINAL::IDENTIFIER,
        in_layout,
        out,
    );
    assert_eq!(res, Poll::Ready(Ok(())));
    assert!(is_terminal);
}

// -----------------------------------------------------------------------------
// Output translation: `\n` is written as `\r\n`
// -----------------------------------------------------------------------------

/// Accepts at most `limit` bytes per write, then reports itself full once.
struct LimitedOutputDevice {
    output: std::sync::Mutex<Vec<u8>>,
    limit: usize,
    full_once: core::sync::atomic::AtomicBool,
}

impl DirectBaseOperations for LimitedOutputDevice {
    fn read(
        &self,
        _context: &mut Context<'_>,
        _buffer: &mut [u8],
        _absolute_position: Size,
    ) -> Poll<Result<usize>> {
        Poll::Ready(Err(IoErrorKind::Unsupported))
    }

    fn write(
        &self,
        _context: &mut Context<'_>,
        buffer: &[u8],
        _absolute_position: Size,
    ) -> Poll<Result<usize>> {
        if self.full_once.swap(false, Ordering::SeqCst) {
            return Poll::Pending;
        }
        let count = buffer.len().min(self.limit);
        self.output
            .lock()
            .unwrap()
            .extend_from_slice(&buffer[..count]);
        if count < buffer.len() {
            self.full_once.store(true, Ordering::SeqCst);
        }
        Poll::Ready(Ok(count))
    }
}

impl MountOperations for LimitedOutputDevice {}
impl DirectCharacterDevice for LimitedOutputDevice {}

fn write_all<D: DirectBaseOperations>(device: &D, mut buffer: &[u8]) {
    let waker = Waker::noop();
    let mut context = Context::from_waker(waker);
    let mut attempts = 0;
    while !buffer.is_empty() {
        attempts += 1;
        assert!(attempts < 1000, "write made no progress");
        if let Poll::Ready(result) = device.write(&mut context, buffer, 0) {
            buffer = &buffer[result.unwrap()..];
        }
    }
}

#[test]
fn output_translates_line_feeds() {
    let terminal = TerminalOutputDevice::new(RecordingOutputDevice::new());
    write_all(&terminal, b"first\nsecond\n\nthird");
    assert_eq!(terminal.inner().recorded(), b"first\r\nsecond\r\n\r\nthird");
}

#[test]
fn output_without_line_feeds_is_unchanged() {
    let terminal = TerminalOutputDevice::new(RecordingOutputDevice::new());
    write_all(&terminal, b"\x1b[0;46m I \x1b[0m | text");
    assert_eq!(terminal.inner().recorded(), b"\x1b[0;46m I \x1b[0m | text");
}

#[test]
fn output_survives_partial_inner_writes() {
    let terminal = TerminalOutputDevice::new(LimitedOutputDevice {
        output: std::sync::Mutex::new(Vec::new()),
        limit: 3,
        full_once: core::sync::atomic::AtomicBool::new(false),
    });
    write_all(&terminal, b"ab\ncdef\n");
    let recorded = terminal.inner().output.lock().unwrap().clone();
    // A line feed split after its carriage return is sent again in full.
    let without_repeated_carriage_returns: Vec<u8> = recorded
        .windows(2)
        .filter(|pair| pair != b"\r\r")
        .map(|pair| pair[0])
        .chain(recorded.last().copied())
        .collect();
    assert_eq!(without_repeated_carriage_returns, b"ab\r\ncdef\r\n");
}

#[test]
fn output_is_pending_when_inner_is_pending_before_any_progress() {
    let terminal = TerminalOutputDevice::new(StalledOutputDevice);
    let waker = Waker::noop();
    let mut context = Context::from_waker(waker);
    assert_eq!(terminal.write(&mut context, b"text\n", 0), Poll::Pending);
}

// -----------------------------------------------------------------------------
// Terminal attributes: echo off and raw (non-canonical) input
// -----------------------------------------------------------------------------

fn without_echo() -> TerminalAttributes {
    TerminalAttributes::INTERACTIVE.with_echo(false)
}

#[test]
fn editor_starts_interactive() {
    let editor = LineEditor::<256>::new();
    assert_eq!(editor.attributes(), TerminalAttributes::INTERACTIVE);
}

#[test]
fn echo_off_hides_typed_characters_but_ends_the_line() {
    let mut editor = LineEditor::<256>::new();
    editor.set_attributes(without_echo());
    let mut echo = Vec::new();

    let result = editor.feed_slice("sécret\r".as_bytes(), &mut echo);
    assert!(result.line_ready);
    assert_eq!(echo, b"\r\n");

    let mut buffer = [0u8; 32];
    let count = editor.pop_completed(&mut buffer);
    assert_eq!(&buffer[..count], "sécret\n".as_bytes());
}

#[test]
fn echo_off_still_edits_the_line_silently() {
    let mut editor = LineEditor::<256>::new();
    editor.set_attributes(without_echo());
    let mut echo = Vec::new();

    // Backspace, Ctrl-U, Tab and an escape sequence are applied without output.
    editor.feed_slice(b"ab\x08c\x15x\ty\x1b[D\r\n", &mut echo);
    assert_eq!(echo, b"\r\n");

    let mut buffer = [0u8; 32];
    let count = editor.pop_completed(&mut buffer);
    assert_eq!(&buffer[..count], b"x y\n");
}

#[test]
fn echo_off_ctrl_c_writes_only_the_line_break() {
    let mut editor = LineEditor::<256>::new();
    editor.set_attributes(without_echo());
    let mut echo = Vec::new();

    let result = editor.feed_slice(b"secret\x03", &mut echo);
    assert!(result.line_ready);
    assert_eq!(echo, b"\r\n");

    let mut buffer = [0u8; 32];
    let count = editor.pop_completed(&mut buffer);
    assert_eq!(&buffer[..count], b"\n");
}

#[test]
fn echo_off_does_not_ring_the_bell_on_overflow() {
    let mut editor = LineEditor::<4>::new();
    editor.set_attributes(without_echo());
    let mut echo = Vec::new();

    editor.feed_slice(b"abcdef", &mut echo);
    assert!(echo.is_empty());
    assert_eq!(editor.current_line(), b"abcd");
}

#[test]
fn echo_back_on_restores_echo() {
    let mut editor = LineEditor::<256>::new();
    let mut echo = Vec::new();

    editor.set_attributes(without_echo());
    editor.feed_slice(b"secret\r", &mut echo);
    editor.set_attributes(TerminalAttributes::INTERACTIVE);
    echo.clear();
    editor.feed_slice(b"ls\r", &mut echo);
    assert_eq!(echo, b"ls\r\n");

    let mut buffer = [0u8; 32];
    let count = editor.pop_completed(&mut buffer);
    assert_eq!(&buffer[..count], b"secret\nls\n");
}

#[test]
fn raw_mode_delivers_every_byte_unchanged_and_at_once() {
    let mut editor = LineEditor::<256>::new();
    editor.set_attributes(TerminalAttributes::RAW);
    let mut echo = Vec::new();

    let input = b"a\r\x03\x04\x08\x7f\x15\x1b[A\n";
    for &byte in input {
        let result = editor.feed_byte(byte, &mut echo);
        assert!(result.line_ready);
        assert!(!result.eof);
    }
    assert!(echo.is_empty());
    assert!(!editor.has_eof());

    let mut buffer = [0u8; 32];
    let count = editor.pop_completed(&mut buffer);
    assert_eq!(&buffer[..count], input);
}

#[test]
fn raw_mode_with_echo_writes_bytes_back_verbatim() {
    let mut editor = LineEditor::<256>::new();
    editor.set_attributes(TerminalAttributes::INTERACTIVE.with_canonical(false));
    let mut echo = Vec::new();

    editor.feed_slice(b"q\r\x08", &mut echo);
    assert_eq!(echo, b"q\r\x08");
}

#[test]
fn raw_mode_drops_bytes_when_the_reader_queue_is_full() {
    let mut editor = LineEditor::<16, 4>::new();
    editor.set_attributes(TerminalAttributes::INTERACTIVE.with_canonical(false));
    let mut echo = Vec::new();

    editor.feed_slice(b"abcdef", &mut echo);
    // Dropped bytes are not echoed either.
    assert_eq!(echo, b"abcd");

    let mut buffer = [0u8; 8];
    let count = editor.pop_completed(&mut buffer);
    assert_eq!(&buffer[..count], b"abcd");
}

#[test]
fn full_queue_rejects_a_line_and_keeps_the_queued_bytes() {
    // The terminal policy is to reject what does not fit, never to drop queued bytes.
    let mut editor = LineEditor::<16, 6>::new();
    let mut echo = Vec::new();

    assert!(editor.feed_slice(b"abc\n", &mut echo).line_ready);
    echo.clear();

    // "defg\n" needs five bytes and only two are left: the line is kept, the bell rings.
    let result = editor.feed_slice(b"defg\n", &mut echo);
    assert!(!result.line_ready);
    assert_eq!(echo, b"defg\x07");
    assert_eq!(editor.current_line(), b"defg");
    assert_eq!(editor.completed_len(), 4);

    // Once the reader drains the queue, the same line commits whole.
    let mut buffer = [0u8; 8];
    let count = editor.pop_completed(&mut buffer);
    assert_eq!(&buffer[..count], b"abc\n");
    assert!(editor.feed_byte(b'\n', &mut echo).line_ready);
    let count = editor.pop_completed(&mut buffer);
    assert_eq!(&buffer[..count], b"defg\n");
}

#[test]
fn ctrl_c_on_a_full_queue_drops_the_oldest_byte() {
    // Ctrl-C is the one input that overwrites: the oldest queued byte makes room.
    let mut editor = LineEditor::<16, 4>::new();
    let mut echo = Vec::new();

    editor.feed_slice(b"abc\n", &mut echo);
    assert_eq!(editor.completed_len(), 4);

    echo.clear();
    let result = editor.feed_byte(0x03, &mut echo);
    assert!(result.line_ready);
    assert_eq!(echo, b"^C\r\n");
    assert_eq!(editor.completed_len(), 4);

    let mut buffer = [0u8; 8];
    let count = editor.pop_completed(&mut buffer);
    assert_eq!(&buffer[..count], b"bc\n\n");
}

#[test]
fn leaving_canonical_mode_releases_the_partial_line() {
    let mut editor = LineEditor::<256>::new();
    let mut echo = Vec::new();

    editor.feed_slice(b"first\npart", &mut echo);
    editor.set_attributes(TerminalAttributes::RAW);
    assert_eq!(editor.current_line(), b"");
    editor.feed_slice(b"!", &mut echo);

    let mut buffer = [0u8; 32];
    let count = editor.pop_completed(&mut buffer);
    assert_eq!(&buffer[..count], b"first\npart!");
}

#[test]
fn returning_to_canonical_mode_edits_lines_again() {
    let mut editor = LineEditor::<256>::new();
    let mut echo = Vec::new();

    editor.set_attributes(TerminalAttributes::RAW);
    editor.feed_slice(b"x\r", &mut echo);
    editor.set_attributes(TerminalAttributes::INTERACTIVE);
    // The '\n' of the raw "\r" must not be swallowed as a CR LF pair.
    let result = editor.feed_slice(b"\nab\x08c\r", &mut echo);
    assert!(result.line_ready);
    assert_eq!(echo, b"\r\nab\x08 \x08c\r\n");

    let mut buffer = [0u8; 32];
    let count = editor.pop_completed(&mut buffer);
    assert_eq!(&buffer[..count], b"x\r\nac\n");
}

fn get_attributes<D: DirectBaseOperations>(device: &D) -> TerminalAttributes {
    let mut attributes = TerminalAttributes::RAW;
    let waker = Waker::noop();
    let mut context = Context::from_waker(waker);
    let result = device.control(
        &mut context,
        GET_TERMINAL_ATTRIBUTES::IDENTIFIER,
        AnyByLayout::from(&()),
        AnyByLayout::from_mutable(&mut attributes),
    );
    assert_eq!(result, Poll::Ready(Ok(())));
    attributes
}

fn set_attributes<D: DirectBaseOperations + ?Sized>(
    device: &D,
    attributes: TerminalAttributes,
) -> Poll<Result<()>> {
    let waker = Waker::noop();
    let mut context = Context::from_waker(waker);
    device.control(
        &mut context,
        SET_TERMINAL_ATTRIBUTES::IDENTIFIER,
        AnyByLayout::from(&attributes),
        AnyByLayout::from_mutable(&mut ()),
    )
}

#[test]
fn device_gets_and_sets_terminal_attributes() {
    let terminal = TerminalInputDevice::new(
        ScriptedInputDevice::new(vec![]),
        RecordingOutputDevice::new(),
    );
    assert_eq!(get_attributes(&terminal), TerminalAttributes::INTERACTIVE);

    assert_eq!(
        set_attributes(&terminal, without_echo()),
        Poll::Ready(Ok(()))
    );
    assert_eq!(get_attributes(&terminal), without_echo());

    assert_eq!(
        set_attributes(&terminal, TerminalAttributes::INTERACTIVE),
        Poll::Ready(Ok(()))
    );
    assert_eq!(get_attributes(&terminal), TerminalAttributes::INTERACTIVE);
}

#[test]
fn device_rejects_unknown_terminal_flags() {
    let terminal = TerminalInputDevice::new(
        ScriptedInputDevice::new(vec![]),
        RecordingOutputDevice::new(),
    );
    let unknown = TerminalAttributes::new(TerminalFlags::from_bits_retain(1 << 31));
    assert_eq!(
        set_attributes(&terminal, unknown),
        Poll::Ready(Err(IoErrorKind::InvalidArgument))
    );
    assert_eq!(get_attributes(&terminal), TerminalAttributes::INTERACTIVE);
}

#[test]
fn device_reads_a_password_without_echo() {
    let terminal = TerminalInputDevice::new(
        ScriptedInputDevice::new(vec![b"hunter2\r"]),
        RecordingOutputDevice::new(),
    );
    assert_eq!(
        set_attributes(&terminal, without_echo()),
        Poll::Ready(Ok(()))
    );

    let mut buffer = [0u8; 32];
    let waker = Waker::noop();
    let mut context = Context::from_waker(waker);
    assert_eq!(
        terminal.read(&mut context, &mut buffer, 0),
        Poll::Ready(Ok(8))
    );
    assert_eq!(&buffer[..8], b"hunter2\n");
    assert_eq!(terminal.inner_out().recorded(), b"\r\n");
}

#[test]
fn device_raw_mode_returns_bytes_without_waiting_for_enter() {
    let terminal = TerminalInputDevice::new(
        ScriptedInputDevice::new(vec![b"\x1b[A"]),
        RecordingOutputDevice::new(),
    );
    assert_eq!(
        set_attributes(&terminal, TerminalAttributes::RAW),
        Poll::Ready(Ok(()))
    );

    let mut buffer = [0u8; 32];
    let waker = Waker::noop();
    let mut context = Context::from_waker(waker);
    assert_eq!(
        terminal.read(&mut context, &mut buffer, 0),
        Poll::Ready(Ok(3))
    );
    assert_eq!(&buffer[..3], b"\x1b[A");
    assert!(terminal.inner_out().recorded().is_empty());

    // Nothing more is buffered: the next read waits for the inner device.
    assert_eq!(terminal.read(&mut context, &mut buffer, 0), Poll::Pending);
}

#[test]
fn device_forwards_other_control_commands_to_the_input_device() {
    let terminal = TerminalInputDevice::new(
        ScriptedInputDevice::new(vec![]),
        RecordingOutputDevice::new(),
    );
    let mut block_size = 0u32;
    let waker = Waker::noop();
    let mut context = Context::from_waker(waker);
    // The scripted device keeps the default control, which rejects every command.
    assert_eq!(
        terminal.control(
            &mut context,
            file_system::block_device::GET_BLOCK_SIZE::IDENTIFIER,
            AnyByLayout::from(&()),
            AnyByLayout::from_mutable(&mut block_size),
        ),
        Poll::Ready(Err(IoErrorKind::Unsupported))
    );
}

// -----------------------------------------------------------------------------
// Type-ahead: a raw chunk is fed one line at a time
// -----------------------------------------------------------------------------

fn read_now<D: DirectBaseOperations>(device: &D, buffer: &mut [u8]) -> Poll<Result<usize>> {
    let mut context = Context::from_waker(Waker::noop());
    device.read(&mut context, buffer, 0)
}

#[test]
fn device_typed_ahead_password_is_not_echoed() {
    // The user name and the password arrive in one raw chunk, before the
    // shell turns echo off at the password prompt.
    let terminal = TerminalInputDevice::new(
        ScriptedInputDevice::new(vec![b"admin\rhunter2\r"]),
        RecordingOutputDevice::new(),
    );
    let mut buffer = [0u8; 32];

    assert_eq!(read_now(&terminal, &mut buffer), Poll::Ready(Ok(6)));
    assert_eq!(&buffer[..6], b"admin\n");
    assert_eq!(terminal.inner_out().recorded(), b"admin\r\n");

    assert_eq!(
        set_attributes(&terminal, without_echo()),
        Poll::Ready(Ok(()))
    );
    assert_eq!(read_now(&terminal, &mut buffer), Poll::Ready(Ok(8)));
    assert_eq!(&buffer[..8], b"hunter2\n");
    // Only the line break of the password line is written.
    assert_eq!(terminal.inner_out().recorded(), b"admin\r\n\r\n");
}

#[test]
fn device_typed_ahead_lines_keep_carriage_return_line_feed_pairs() {
    let terminal = TerminalInputDevice::new(
        ScriptedInputDevice::new(vec![b"a\r\nb\r\n"]),
        RecordingOutputDevice::new(),
    );
    let mut buffer = [0u8; 32];

    assert_eq!(read_now(&terminal, &mut buffer), Poll::Ready(Ok(2)));
    assert_eq!(&buffer[..2], b"a\n");
    // The line feed kept for the next read is still paired with its carriage return.
    assert_eq!(read_now(&terminal, &mut buffer), Poll::Ready(Ok(2)));
    assert_eq!(&buffer[..2], b"b\n");
    assert_eq!(read_now(&terminal, &mut buffer), Poll::Pending);
    assert_eq!(terminal.inner_out().recorded(), b"a\r\nb\r\n");
}

#[test]
fn device_carriage_return_line_feed_pair_leaves_nothing_for_raw_mode() {
    let terminal = TerminalInputDevice::new(
        ScriptedInputDevice::new(vec![b"app\r\n"]),
        RecordingOutputDevice::new(),
    );
    let mut buffer = [0u8; 32];

    assert_eq!(read_now(&terminal, &mut buffer), Poll::Ready(Ok(4)));
    assert_eq!(&buffer[..4], b"app\n");

    assert_eq!(
        set_attributes(&terminal, TerminalAttributes::RAW),
        Poll::Ready(Ok(()))
    );
    // The line feed of the Enter key went with its line: no stray byte is left.
    assert_eq!(read_now(&terminal, &mut buffer), Poll::Pending);
}

#[test]
fn device_typed_ahead_bytes_follow_a_switch_to_raw_mode() {
    let terminal = TerminalInputDevice::new(
        ScriptedInputDevice::new(vec![b"ls\rq\x03"]),
        RecordingOutputDevice::new(),
    );
    let mut buffer = [0u8; 32];

    assert_eq!(read_now(&terminal, &mut buffer), Poll::Ready(Ok(3)));
    assert_eq!(&buffer[..3], b"ls\n");

    assert_eq!(
        set_attributes(&terminal, TerminalAttributes::RAW),
        Poll::Ready(Ok(()))
    );
    // The rest of the chunk is delivered raw: Ctrl-C is not interpreted.
    assert_eq!(read_now(&terminal, &mut buffer), Poll::Ready(Ok(2)));
    assert_eq!(&buffer[..2], b"q\x03");
}

#[test]
fn device_typed_ahead_end_of_input_comes_before_the_next_line() {
    let terminal = TerminalInputDevice::new(
        ScriptedInputDevice::new(vec![b"\x04next\r"]),
        RecordingOutputDevice::new(),
    );
    let mut buffer = [0u8; 32];

    assert_eq!(read_now(&terminal, &mut buffer), Poll::Ready(Ok(0)));
    assert_eq!(read_now(&terminal, &mut buffer), Poll::Ready(Ok(5)));
    assert_eq!(&buffer[..5], b"next\n");
}

#[test]
fn editor_feeds_until_the_end_of_the_first_line() {
    let mut editor = LineEditor::<256>::new();
    let mut echo = Vec::new();

    assert_eq!(editor.feed_until_line_end(b"ab\rcd\r", &mut echo), 3);
    assert_eq!(editor.feed_until_line_end(b"e\r\nf\r", &mut echo), 3);
    assert_eq!(editor.feed_until_line_end(b"x\x03y", &mut echo), 2);
    assert_eq!(editor.feed_until_line_end(b"\x04z", &mut echo), 1);
    assert_eq!(editor.feed_until_line_end(b"partial", &mut echo), 7);

    editor.set_attributes(TerminalAttributes::RAW);
    assert_eq!(editor.feed_until_line_end(b"r\rs\r", &mut echo), 4);
}

// -----------------------------------------------------------------------------
// Mode switches reset the canonical line state
// -----------------------------------------------------------------------------

#[test]
fn mode_switch_forgets_a_pending_carriage_return() {
    let mut editor = LineEditor::<256>::new();
    let mut echo = Vec::new();

    // The carriage return would swallow a following line feed as a CR LF pair.
    editor.feed_slice(b"a\r", &mut echo);
    editor.set_attributes(TerminalAttributes::RAW);
    editor.set_attributes(TerminalAttributes::INTERACTIVE);

    // After the switch, a line feed ends a new, empty line.
    echo.clear();
    let result = editor.feed_slice(b"\n", &mut echo);
    assert!(result.line_ready);
    assert_eq!(echo, b"\r\n");

    let mut buffer = [0u8; 32];
    let count = editor.pop_completed(&mut buffer);
    assert_eq!(&buffer[..count], b"a\n\n");
}

#[test]
fn mode_switch_forgets_a_partial_escape_sequence() {
    let mut editor = LineEditor::<256>::new();
    let mut echo = Vec::new();

    // A partial control sequence would swallow the next byte as its final byte.
    editor.feed_slice(b"\x1b[", &mut echo);
    editor.set_attributes(TerminalAttributes::RAW);
    editor.set_attributes(TerminalAttributes::INTERACTIVE);

    let result = editor.feed_slice(b"A\r", &mut echo);
    assert!(result.line_ready);
    assert_eq!(echo, b"A\r\n");

    let mut buffer = [0u8; 32];
    let count = editor.pop_completed(&mut buffer);
    assert_eq!(&buffer[..count], b"A\n");
}

// -----------------------------------------------------------------------------
// A pending reader is woken when a mode switch releases input
// -----------------------------------------------------------------------------

#[derive(Default)]
struct CountingWaker {
    wakes: AtomicUsize,
}

impl CountingWaker {
    fn wakes(&self) -> usize {
        self.wakes.load(Ordering::SeqCst)
    }
}

impl std::task::Wake for CountingWaker {
    fn wake(self: std::sync::Arc<Self>) {
        self.wake_by_ref();
    }

    fn wake_by_ref(self: &std::sync::Arc<Self>) {
        self.wakes.fetch_add(1, Ordering::SeqCst);
    }
}

#[test]
fn device_switch_to_raw_mode_wakes_the_pending_reader() {
    let terminal = TerminalInputDevice::new(
        ScriptedInputDevice::new(vec![b"part"]),
        RecordingOutputDevice::new(),
    );
    let counting_waker = std::sync::Arc::new(CountingWaker::default());
    let waker = Waker::from(counting_waker.clone());
    let mut context = Context::from_waker(&waker);
    let mut buffer = [0u8; 32];

    // The partial line is buffered and the reader waits for Enter.
    assert_eq!(terminal.read(&mut context, &mut buffer, 0), Poll::Pending);

    // Turning echo off releases nothing: the reader is not woken.
    assert_eq!(
        set_attributes(&terminal, without_echo()),
        Poll::Ready(Ok(()))
    );
    assert_eq!(counting_waker.wakes(), 0);

    // Leaving canonical mode releases the partial line and wakes the reader.
    assert_eq!(
        set_attributes(&terminal, TerminalAttributes::RAW),
        Poll::Ready(Ok(()))
    );
    assert_eq!(counting_waker.wakes(), 1);

    // The waker was taken, not cloned: another switch while the released
    // line is still readable does not wake the reader again.
    assert_eq!(
        set_attributes(&terminal, TerminalAttributes::RAW),
        Poll::Ready(Ok(()))
    );
    assert_eq!(counting_waker.wakes(), 1);

    assert_eq!(
        terminal.read(&mut context, &mut buffer, 0),
        Poll::Ready(Ok(4))
    );
    assert_eq!(&buffer[..4], b"part");

    // Back in canonical mode, the reader waits for a new line, then reads its
    // first byte when it is completed.
    assert_eq!(
        set_attributes(&terminal, TerminalAttributes::INTERACTIVE),
        Poll::Ready(Ok(()))
    );
    terminal.inner_in().push_chunk(b"next");
    assert_eq!(terminal.read(&mut context, &mut buffer, 0), Poll::Pending);
    terminal.inner_in().push_chunk(b"\r");
    assert_eq!(
        terminal.read(&mut context, &mut buffer[..1], 0),
        Poll::Ready(Ok(1))
    );
    assert_eq!(&buffer[..1], b"n");

    // The rest of the line is still queued, but the read that returned data
    // cleared the waker: a switch now wakes no one.
    assert_eq!(
        set_attributes(&terminal, without_echo()),
        Poll::Ready(Ok(()))
    );
    assert_eq!(counting_waker.wakes(), 1);
}

/// Raw input device that, once its single chunk is read, switches the terminal
/// wrapping it to raw mode from inside its next `read`, just before answering
/// `Pending`: the switch lands after the terminal checked for readable input
/// and before it registered its waker.
struct SwitchingInputDevice {
    chunk: std::sync::Mutex<Option<Vec<u8>>>,
    terminal: std::sync::OnceLock<&'static dyn DirectBaseOperations>,
}

impl DirectBaseOperations for SwitchingInputDevice {
    fn read(
        &self,
        _context: &mut Context<'_>,
        buffer: &mut [u8],
        _absolute_position: Size,
    ) -> Poll<Result<usize>> {
        if let Some(chunk) = self.chunk.lock().unwrap().take() {
            buffer[..chunk.len()].copy_from_slice(&chunk);
            return Poll::Ready(Ok(chunk.len()));
        }
        let terminal = self.terminal.get().expect("the terminal is set");
        assert_eq!(
            set_attributes(*terminal, TerminalAttributes::RAW),
            Poll::Ready(Ok(()))
        );
        Poll::Pending
    }

    fn write(
        &self,
        _context: &mut Context<'_>,
        _buffer: &[u8],
        _absolute_position: Size,
    ) -> Poll<Result<usize>> {
        Poll::Ready(Err(IoErrorKind::Unsupported))
    }
}

impl MountOperations for SwitchingInputDevice {}
impl DirectCharacterDevice for SwitchingInputDevice {}

#[test]
fn device_switch_to_raw_mode_during_the_inner_read_returns_the_partial_line() {
    let terminal: &'static TerminalInputDevice<SwitchingInputDevice, RecordingOutputDevice> =
        std::boxed::Box::leak(std::boxed::Box::new(TerminalInputDevice::new(
            SwitchingInputDevice {
                chunk: std::sync::Mutex::new(Some(b"part".to_vec())),
                terminal: std::sync::OnceLock::new(),
            },
            RecordingOutputDevice::new(),
        )));
    assert!(terminal.inner_in().terminal.set(terminal).is_ok());
    let mut buffer = [0u8; 32];

    // No wake-up would follow a `Pending` answer: the switch happened before
    // the waker was registered, so the terminal checks again after registering.
    assert_eq!(read_now(terminal, &mut buffer), Poll::Ready(Ok(4)));
    assert_eq!(&buffer[..4], b"part");
}
