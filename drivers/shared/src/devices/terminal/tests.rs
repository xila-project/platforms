use alloc::vec;
use alloc::vec::Vec;
use core::sync::atomic::{AtomicUsize, Ordering};
use core::task::{Context, Poll, Waker};
use file_system::{
    ControlCommand, ControlCommandIdentifier, DirectBaseOperations, DirectCharacterDevice, Error,
    MountOperations, Result, Size,
};
use shared::AnyByLayout;

use super::device::TerminalInputDevice;
use super::logic::{EchoBuffer, EchoSink, LineEditor};

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
}

impl DirectBaseOperations for ScriptedInputDevice {
    fn read(
        &self,
        _cx: &mut Context<'_>,
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
        _cx: &mut Context<'_>,
        _buffer: &[u8],
        _absolute_position: Size,
    ) -> Poll<Result<usize>> {
        Poll::Ready(Err(Error::UnsupportedOperation))
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
        _cx: &mut Context<'_>,
        _buffer: &mut [u8],
        _absolute_position: Size,
    ) -> Poll<Result<usize>> {
        Poll::Ready(Err(Error::UnsupportedOperation))
    }

    fn write(
        &self,
        _cx: &mut Context<'_>,
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
        _cx: &mut Context<'_>,
        _buffer: &mut [u8],
        _absolute_position: Size,
    ) -> Poll<Result<usize>> {
        Poll::Ready(Err(Error::UnsupportedOperation))
    }

    fn write(
        &self,
        _cx: &mut Context<'_>,
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
    let mut cx = Context::from_waker(waker);

    // Initial read: pulls "hel", then pulls "lo", neither completes line.
    // Script is exhausted, inner input returns Pending, registering waker.
    // TerminalInputDevice returns Pending!
    let poll_res = terminal.read(&mut cx, &mut buf, 0);
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
    let mut cx = Context::from_waker(waker);

    let poll_res = terminal.read(&mut cx, &mut buf, 0);
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
    let mut cx = Context::from_waker(waker);

    let poll_res = terminal.read(&mut cx, &mut buf, 0);
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
    let mut cx = Context::from_waker(waker);

    // Output write returns Pending, which is dropped per echo policy.
    // Line delivery succeeds without stalling!
    let poll_res = terminal.read(&mut cx, &mut buf, 0);
    assert_eq!(poll_res, Poll::Ready(Ok(8)));
    assert_eq!(&buf[..8], b"command\n");
}

#[test]
fn device_control_forwarding() {
    struct MockTerminalDevice;
    impl DirectBaseOperations for MockTerminalDevice {
        fn read(&self, _cx: &mut Context<'_>, _buf: &mut [u8], _pos: Size) -> Poll<Result<usize>> {
            Poll::Pending
        }
        fn write(&self, _cx: &mut Context<'_>, _buf: &[u8], _pos: Size) -> Poll<Result<usize>> {
            Poll::Ready(Ok(0))
        }
        fn control(
            &self,
            _cx: &mut Context<'_>,
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
                Poll::Ready(Err(Error::UnsupportedOperation))
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
    let mut cx = Context::from_waker(waker);

    let res = terminal.control(
        &mut cx,
        file_system::character_device::IS_A_TERMINAL::IDENTIFIER,
        in_layout,
        out,
    );
    assert_eq!(res, Poll::Ready(Ok(())));
    assert!(is_terminal);
}
