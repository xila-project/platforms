//! Terminal line editor state machine (Layer 1).
//!
//! Provides POSIX cooked-mode (canonical mode) line editing over raw serial input
//! with a fixed-capacity buffer and no heap allocations.

/// Sink for receiving echo bytes emitted by the line editor.
pub trait EchoSink {
    /// Emit echo bytes to the sink.
    fn emit(&mut self, bytes: &[u8]);
}

impl<F: FnMut(&[u8])> EchoSink for F {
    fn emit(&mut self, bytes: &[u8]) {
        self(bytes);
    }
}

impl EchoSink for alloc::vec::Vec<u8> {
    fn emit(&mut self, bytes: &[u8]) {
        self.extend_from_slice(bytes);
    }
}

/// A fixed-capacity buffer for collecting echo output without heap allocation.
#[derive(Debug, Clone, Copy)]
pub struct EchoBuffer<const CAP: usize> {
    storage: [u8; CAP],
    len: usize,
}

impl<const CAP: usize> Default for EchoBuffer<CAP> {
    fn default() -> Self {
        Self::new()
    }
}

impl<const CAP: usize> EchoBuffer<CAP> {
    /// Create a new, empty `EchoBuffer`.
    pub const fn new() -> Self {
        Self {
            storage: [0; CAP],
            len: 0,
        }
    }

    /// Return the populated bytes as a slice.
    pub fn as_bytes(&self) -> &[u8] {
        &self.storage[..self.len]
    }

    /// Clear the buffer.
    pub fn clear(&mut self) {
        self.len = 0;
    }

    /// Number of bytes currently stored.
    pub const fn len(&self) -> usize {
        self.len
    }

    /// Whether the buffer is empty.
    pub const fn is_empty(&self) -> bool {
        self.len == 0
    }
}

impl<const CAP: usize> EchoSink for EchoBuffer<CAP> {
    fn emit(&mut self, bytes: &[u8]) {
        let to_copy = bytes.len().min(CAP - self.len);
        self.storage[self.len..self.len + to_copy].copy_from_slice(&bytes[..to_copy]);
        self.len += to_copy;
    }
}

/// Fixed-capacity ring buffer for FIFO queuing of completed lines.
#[derive(Debug, Clone, Copy)]
pub struct RingBuffer<const N: usize> {
    storage: [u8; N],
    read_pos: usize,
    write_pos: usize,
    len: usize,
}

impl<const N: usize> Default for RingBuffer<N> {
    fn default() -> Self {
        Self::new()
    }
}

impl<const N: usize> RingBuffer<N> {
    /// Create an empty `RingBuffer`.
    pub const fn new() -> Self {
        Self {
            storage: [0; N],
            read_pos: 0,
            write_pos: 0,
            len: 0,
        }
    }

    /// Total capacity in bytes.
    pub const fn capacity(&self) -> usize {
        N
    }

    /// Current number of stored bytes.
    pub const fn len(&self) -> usize {
        self.len
    }

    /// Whether the ring buffer is empty.
    pub const fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// Whether the ring buffer is completely full.
    pub const fn is_full(&self) -> bool {
        self.len == N
    }

    /// Remaining available space in bytes.
    pub const fn available_space(&self) -> usize {
        N - self.len
    }

    /// Drop the oldest `count` bytes from the front.
    pub fn drop_oldest(&mut self, count: usize) {
        let to_drop = count.min(self.len);
        self.read_pos = (self.read_pos + to_drop) % N;
        self.len -= to_drop;
    }

    /// Push a single byte into the ring buffer.
    ///
    /// Returns `true` if pushed, `false` if full.
    pub fn push_byte(&mut self, byte: u8) -> bool {
        if self.is_full() {
            return false;
        }
        self.storage[self.write_pos] = byte;
        self.write_pos = (self.write_pos + 1) % N;
        self.len += 1;
        true
    }

    /// Push a slice of bytes into the ring buffer, up to available space.
    ///
    /// Returns the number of bytes pushed.
    pub fn push_slice(&mut self, data: &[u8]) -> usize {
        let to_push = data.len().min(self.available_space());
        for &byte in &data[..to_push] {
            self.storage[self.write_pos] = byte;
            self.write_pos = (self.write_pos + 1) % N;
        }
        self.len += to_push;
        to_push
    }

    /// Pop a single byte from the front of the ring buffer.
    pub fn pop_byte(&mut self) -> Option<u8> {
        if self.is_empty() {
            None
        } else {
            let byte = self.storage[self.read_pos];
            self.read_pos = (self.read_pos + 1) % N;
            self.len -= 1;
            Some(byte)
        }
    }

    /// Pop up to `out.len()` bytes into `out`.
    ///
    /// Returns the number of bytes popped.
    pub fn pop_slice(&mut self, out: &mut [u8]) -> usize {
        let to_read = out.len().min(self.len);
        for byte in &mut out[..to_read] {
            *byte = self.storage[self.read_pos];
            self.read_pos = (self.read_pos + 1) % N;
        }
        self.len -= to_read;
        to_read
    }
}

/// ANSI escape sequence state machine.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum EscapeState {
    #[default]
    None,
    Esc,
    Csi,
    Ss3,
}

/// Result returned after processing an input byte.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct ProcessResult {
    /// A complete line (or EOF) is now available in the completed buffer.
    pub line_ready: bool,
    /// An end-of-input (Ctrl-D on empty line) was reported.
    pub eof: bool,
}

impl ProcessResult {
    /// Construct an empty result indicating no line is completed yet.
    pub const fn empty() -> Self {
        Self {
            line_ready: false,
            eof: false,
        }
    }
}

/// UTF-8 character length from its leading byte.
const fn utf8_char_len(first_byte: u8) -> Option<usize> {
    if first_byte < 0x80 {
        Some(1)
    } else if first_byte >= 0xC2 && first_byte <= 0xDF {
        Some(2)
    } else if first_byte >= 0xE0 && first_byte <= 0xEF {
        Some(3)
    } else if first_byte >= 0xF0 && first_byte <= 0xF4 {
        Some(4)
    } else {
        None
    }
}

/// Canonical mode line editor with fixed-capacity line and type-ahead buffers.
///
/// `N` is the capacity of a single line being edited (default 256 bytes).
/// `Q` is the capacity of the completed line FIFO queue (default 512 bytes).
#[derive(Debug)]
pub struct LineEditor<const N: usize = 256, const Q: usize = 512> {
    line_buf: [u8; N],
    line_len: usize,
    completed: RingBuffer<Q>,
    prev_was_cr: bool,
    escape_state: EscapeState,
    discard_continuation: usize,
    eof: bool,
}

impl<const N: usize, const Q: usize> Default for LineEditor<N, Q> {
    fn default() -> Self {
        Self::new()
    }
}

impl<const N: usize, const Q: usize> LineEditor<N, Q> {
    /// Create a new, empty `LineEditor`.
    pub const fn new() -> Self {
        Self {
            line_buf: [0; N],
            line_len: 0,
            completed: RingBuffer::new(),
            prev_was_cr: false,
            escape_state: EscapeState::None,
            discard_continuation: 0,
            eof: false,
        }
    }

    /// Feed a single input byte into the line editor.
    ///
    /// Echo output is appended to `echo`.
    pub fn feed_byte<E: EchoSink>(&mut self, byte: u8, echo: &mut E) -> ProcessResult {
        // 1. Process escape sequences.
        match self.escape_state {
            EscapeState::Esc => {
                match byte {
                    b'[' => self.escape_state = EscapeState::Csi,
                    b'O' => self.escape_state = EscapeState::Ss3,
                    _ => self.escape_state = EscapeState::None,
                }
                return ProcessResult::empty();
            }
            EscapeState::Csi => {
                if (0x20..=0x3F).contains(&byte) {
                    // Parameter or intermediate byte; keep reading CSI sequence.
                } else if (0x40..=0x7E).contains(&byte) {
                    // Final byte completes the CSI sequence.
                    self.escape_state = EscapeState::None;
                } else if byte == 0x1B {
                    self.escape_state = EscapeState::Esc;
                } else {
                    self.escape_state = EscapeState::None;
                }
                return ProcessResult::empty();
            }
            EscapeState::Ss3 => {
                // Any single byte completes the SS3 sequence.
                self.escape_state = EscapeState::None;
                return ProcessResult::empty();
            }
            EscapeState::None => {}
        }

        // Start of escape sequence.
        if byte == 0x1B {
            self.escape_state = EscapeState::Esc;
            return ProcessResult::empty();
        }

        // 2. CR / LF pair handling:
        // A lone '\n' following a completed '\r' line is swallowed.
        if self.prev_was_cr {
            self.prev_was_cr = false;
            if byte == b'\n' {
                return ProcessResult::empty();
            }
        }

        // 3. Enter keys: '\r' or '\n'.
        if byte == b'\r' {
            self.prev_was_cr = true;
            return self.commit_line(echo);
        }
        if byte == b'\n' {
            return self.commit_line(echo);
        }

        // 4. Backspace: 0x08 (\b) or 0x7F (DEL).
        if byte == 0x08 || byte == 0x7F {
            if self.line_len > 0 {
                // Pop all continuation bytes of the last UTF-8 character.
                while self.line_len > 0 && (self.line_buf[self.line_len - 1] & 0xC0) == 0x80 {
                    self.line_len -= 1;
                }
                // Pop leading byte of the UTF-8 character.
                if self.line_len > 0 {
                    self.line_len -= 1;
                }
                echo.emit(b"\x08 \x08");
            }
            return ProcessResult::empty();
        }

        // 5. Ctrl-U (0x15): erase current line.
        if byte == 0x15 {
            let char_count = self.line_buf[..self.line_len]
                .iter()
                .filter(|&&b| (b & 0xC0) != 0x80)
                .count();
            self.line_len = 0;
            for _ in 0..char_count {
                echo.emit(b"\x08 \x08");
            }
            return ProcessResult::empty();
        }

        // 6. Ctrl-C (0x03): discard line, echo ^C\r\n, make EMPTY line (\n) available.
        if byte == 0x03 {
            self.line_len = 0;
            echo.emit(b"^C\r\n");
            if self.completed.available_space() >= 1 {
                self.completed.push_byte(b'\n');
            } else {
                // Make room for the empty line delimiter.
                self.completed.drop_oldest(1);
                self.completed.push_byte(b'\n');
            }
            return ProcessResult {
                line_ready: true,
                eof: false,
            };
        }

        // 7. Ctrl-D (0x04): end-of-input on empty line, ignored on non-empty line.
        if byte == 0x04 {
            if self.line_len == 0 {
                self.eof = true;
                return ProcessResult {
                    line_ready: true,
                    eof: true,
                };
            }
            return ProcessResult::empty();
        }

        // 8. Tab (0x09): stored and echoed as a single space.
        if byte == 0x09 {
            if self.line_len < N {
                self.line_buf[self.line_len] = b' ';
                self.line_len += 1;
                echo.emit(b" ");
            } else {
                echo.emit(b"\x07");
            }
            return ProcessResult::empty();
        }

        // 9. Other control bytes below 0x20: ignored.
        if byte < 0x20 {
            return ProcessResult::empty();
        }

        // 10. Discard continuation bytes of an oversized UTF-8 character.
        if self.discard_continuation > 0 {
            if (0x80..=0xBF).contains(&byte) {
                self.discard_continuation -= 1;
                return ProcessResult::empty();
            }
            self.discard_continuation = 0;
        }

        // 11. Printable ASCII (0x20..=0x7E).
        if byte <= 0x7E {
            if self.line_len < N {
                self.line_buf[self.line_len] = byte;
                self.line_len += 1;
                echo.emit(&[byte]);
            } else {
                echo.emit(b"\x07");
            }
            return ProcessResult::empty();
        }

        // 12. UTF-8 continuation byte (0x80..=0xBF) arriving as part of an accepted character.
        if (0x80..=0xBF).contains(&byte) {
            if self.line_len < N {
                self.line_buf[self.line_len] = byte;
                self.line_len += 1;
                echo.emit(&[byte]);
            } else {
                echo.emit(b"\x07");
            }
            return ProcessResult::empty();
        }

        // 13. Multi-byte UTF-8 leading byte (2 to 4 bytes).
        if let Some(char_len) = utf8_char_len(byte) {
            let remaining = N - self.line_len;
            if remaining >= char_len {
                self.line_buf[self.line_len] = byte;
                self.line_len += 1;
                echo.emit(&[byte]);
            } else {
                // Character cannot fit in full; discard leading byte and subsequent continuation bytes.
                self.discard_continuation = char_len - 1;
                echo.emit(b"\x07");
            }
        }

        ProcessResult::empty()
    }

    /// Feed multiple input bytes from a slice into the line editor.
    pub fn feed_slice<E: EchoSink>(&mut self, bytes: &[u8], echo: &mut E) -> ProcessResult {
        let mut any_line_ready = false;
        let mut any_eof = false;
        for &b in bytes {
            let res = self.feed_byte(b, echo);
            any_line_ready |= res.line_ready;
            any_eof |= res.eof;
        }
        ProcessResult {
            line_ready: any_line_ready,
            eof: any_eof,
        }
    }

    /// Commit the current line buffer followed by '\n' to the completed queue.
    fn commit_line<E: EchoSink>(&mut self, echo: &mut E) -> ProcessResult {
        let needed = self.line_len + 1; // Content plus newline
        if self.completed.available_space() >= needed {
            self.completed.push_slice(&self.line_buf[..self.line_len]);
            self.completed.push_byte(b'\n');
            self.line_len = 0;
            echo.emit(b"\r\n");
            ProcessResult {
                line_ready: true,
                eof: false,
            }
        } else {
            // Completed queue is full; preserve current line in buffer and ring the bell.
            echo.emit(b"\x07");
            ProcessResult::empty()
        }
    }

    /// Pop completed line bytes into `buffer`.
    ///
    /// Returns the number of bytes read.
    pub fn pop_completed(&mut self, buffer: &mut [u8]) -> usize {
        self.completed.pop_slice(buffer)
    }

    /// Whether there are bytes of a completed line ready to be read.
    pub fn has_completed_bytes(&self) -> bool {
        !self.completed.is_empty()
    }

    /// Whether an end-of-input (Ctrl-D on empty line) was reported.
    pub const fn has_eof(&self) -> bool {
        self.eof
    }

    /// Check and consume the EOF flag. Returns `true` exactly once per Ctrl-D.
    pub fn take_eof(&mut self) -> bool {
        let was_eof = self.eof;
        self.eof = false;
        was_eof
    }

    /// Current uncommitted line content.
    pub fn current_line(&self) -> &[u8] {
        &self.line_buf[..self.line_len]
    }

    /// Number of completed bytes queued for the reader.
    pub fn completed_len(&self) -> usize {
        self.completed.len()
    }
}
