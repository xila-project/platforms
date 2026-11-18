//! VFS character device wrapping a raw serial console (Layer 2).
//!
//! Provides canonical mode cooked input for the Xila shell and applications,
//! translating raw input into line-buffered input with echo.
//!
//! Programs read and change the line discipline with the
//! `GET_TERMINAL_ATTRIBUTES` and `SET_TERMINAL_ATTRIBUTES` control commands
//! (echo, canonical or raw input); see [`LineEditor`] for their behaviour.
//! Every other control command is forwarded to the raw input device.

use core::cell::RefCell;
use core::task::{Context as TaskContext, Poll, Waker};
use critical_section::Mutex;
use file_system::{
    ControlCommand, ControlCommandIdentifier, DirectBaseOperations, DirectCharacterDevice, Error,
    MountOperations, Operation, Result, Size,
    character_device::{GET_TERMINAL_ATTRIBUTES, SET_TERMINAL_ATTRIBUTES},
};
use shared::AnyByLayout;

use super::logic::LineEditor;

/// Capacity of a raw input chunk, and of the raw bytes kept between reads.
const RAW_CHUNK_CAPACITY: usize = 64;

/// Raw bytes read from the input device but not yet fed to the line editor.
///
/// In canonical mode, a chunk is fed only up to the end of its first line;
/// the rest waits here for the next read, so attribute changes made in between
/// (such as echo turned off before a password prompt) apply to the following
/// line. Its capacity is fixed: nothing is allocated.
#[derive(Debug)]
struct PendingInput {
    bytes: [u8; RAW_CHUNK_CAPACITY],
    start: usize,
    end: usize,
}

impl PendingInput {
    const fn new() -> Self {
        Self {
            bytes: [0; RAW_CHUNK_CAPACITY],
            start: 0,
            end: 0,
        }
    }

    const fn is_empty(&self) -> bool {
        self.start == self.end
    }

    fn as_slice(&self) -> &[u8] {
        &self.bytes[self.start..self.end]
    }

    /// Drop the first `count` bytes, which were fed to the line editor.
    fn consume(&mut self, count: usize) {
        self.start = (self.start + count).min(self.end);
        if self.is_empty() {
            self.start = 0;
            self.end = 0;
        }
    }

    /// Append raw bytes after those still pending.
    ///
    /// The buffer is empty whenever a single reader pulls new raw bytes; with
    /// concurrent readers, bytes that do not fit are dropped, like a full
    /// reader queue does.
    fn append(&mut self, bytes: &[u8]) {
        if self.start > 0 {
            self.bytes.copy_within(self.start..self.end, 0);
            self.end -= self.start;
            self.start = 0;
        }
        let count = bytes.len().min(RAW_CHUNK_CAPACITY - self.end);
        self.bytes[self.end..self.end + count].copy_from_slice(&bytes[..count]);
        self.end += count;
    }
}

/// Line editor, raw input still to be fed to it, and the waiting reader.
#[derive(Debug)]
struct TerminalState<const N: usize, const Q: usize> {
    editor: LineEditor<N, Q>,
    pending_input: PendingInput,
    /// Waker of the last read that returned `Pending`, woken when a change of
    /// attributes makes input readable (leaving canonical mode releases the
    /// partially typed line) without new raw bytes arriving.
    reader_waker: Option<Waker>,
}

impl<const N: usize, const Q: usize> TerminalState<N, Q> {
    const fn new() -> Self {
        Self {
            editor: LineEditor::new(),
            pending_input: PendingInput::new(),
            reader_waker: None,
        }
    }
}

/// VFS character device providing cooked terminal input over raw I/O devices.
///
/// `I` is the raw character input device (e.g. `StandardInDevice`).
/// `O` is the raw character output device for terminal echo (e.g. `StandardOutDevice`).
/// `N` is the maximum line capacity in bytes (default 256).
/// `Q` is the type-ahead completed line queue capacity in bytes (default 512).
pub struct TerminalInputDevice<I, O, const N: usize = 256, const Q: usize = 512> {
    inner_in: I,
    inner_out: O,
    state: Mutex<RefCell<TerminalState<N, Q>>>,
}

impl<I, O, const N: usize, const Q: usize> TerminalInputDevice<I, O, N, Q> {
    /// Create a new `TerminalInputDevice` wrapping raw input and output devices with custom capacities.
    ///
    /// The reader queue capacity `Q` must be greater than the line capacity
    /// `N`, which is checked at compile time: a line is queued with its line
    /// feed, so a full line needs `N + 1` bytes. With a smaller queue, a full
    /// line could never be delivered and its reader would wait forever, and a
    /// queue of 0 bytes would make the queue arithmetic divide by zero.
    pub const fn new_with_capacities(inner_in: I, inner_out: O) -> Self {
        const {
            assert!(
                Q > N,
                "the reader queue must hold a full line and its line feed"
            )
        };
        Self {
            inner_in,
            inner_out,
            state: Mutex::new(RefCell::new(TerminalState::new())),
        }
    }

    /// Access the underlying raw input device.
    pub const fn inner_in(&self) -> &I {
        &self.inner_in
    }

    /// Access the underlying raw output device.
    pub const fn inner_out(&self) -> &O {
        &self.inner_out
    }

    /// Access the editor state within a critical section, returning `Error::RessourceBusy` if contended.
    fn with_state<R>(&self, f: impl FnOnce(&mut TerminalState<N, Q>) -> Result<R>) -> Result<R> {
        critical_section::with(|cs| {
            let mut state = self
                .state
                .borrow(cs)
                .try_borrow_mut()
                .map_err(|_| Error::RessourceBusy)?;
            f(&mut state)
        })
    }
}

impl<I, O: DirectCharacterDevice, const N: usize, const Q: usize> TerminalInputDevice<I, O, N, Q> {
    /// Feed pending raw bytes to the line editor, up to the end of the next
    /// line in canonical mode, and write their echo to the output device.
    fn feed_pending_input(&self, state: &mut TerminalState<N, Q>) {
        let TerminalState {
            editor,
            pending_input,
            ..
        } = state;
        let mut echo = |echo_bytes: &[u8]| {
            let mut noop_context = TaskContext::from_waker(Waker::noop());
            let _ = self.inner_out.write(&mut noop_context, echo_bytes, 0);
        };
        let consumed = editor.feed_until_line_end(pending_input.as_slice(), &mut echo);
        pending_input.consume(consumed);
    }
}

impl<I, O> TerminalInputDevice<I, O, 256, 512> {
    /// Create a new `TerminalInputDevice` wrapping raw input and output devices with default capacities (N=256, Q=512).
    pub const fn new(inner_in: I, inner_out: O) -> Self {
        Self::new_with_capacities(inner_in, inner_out)
    }
}

impl<I: DirectCharacterDevice, O: DirectCharacterDevice, const N: usize, const Q: usize>
    DirectBaseOperations for TerminalInputDevice<I, O, N, Q>
{
    fn read(
        &self,
        context: &mut TaskContext<'_>,
        buffer: &mut [u8],
        _absolute_position: Size,
    ) -> Poll<Result<usize>> {
        if buffer.is_empty() {
            return Poll::Ready(Ok(0));
        }

        loop {
            // 1. Deliver completed lines or EOF if already available in the editor.
            // 2. Otherwise, feed raw bytes left over from an earlier chunk, up to
            //    the end of the next line, before pulling new ones.
            let mut data_to_return = None;
            let mut fed_pending_input = false;
            let lock_res = self.with_state(|state| {
                if state.editor.is_readable() {
                    state.reader_waker = None;
                }
                if state.editor.has_completed_bytes() {
                    let n = state.editor.pop_completed(buffer);
                    data_to_return = Some(Ok(n));
                } else if state.editor.take_eof() {
                    data_to_return = Some(Ok(0));
                } else if !state.pending_input.is_empty() {
                    self.feed_pending_input(state);
                    fed_pending_input = true;
                }
                Ok(())
            });

            if let Err(e) = lock_res {
                return Poll::Ready(Err(e));
            }

            if let Some(res) = data_to_return {
                return Poll::Ready(res);
            }

            if fed_pending_input {
                // The next iteration returns a line if one was completed, or
                // keeps feeding (and eventually pulls new raw bytes) otherwise.
                continue;
            }

            // 3. No completed data available: pull raw bytes from the inner input device.
            let mut raw_buf = [0u8; RAW_CHUNK_CAPACITY];
            match self.inner_in.read(context, &mut raw_buf, 0) {
                Poll::Pending => {
                    // Inner device has registered the waker from context.
                    // Returning Pending guarantees a wake-up when hardware input arrives.
                    // The waker is also kept here for a change of attributes
                    // that releases the partial line; if that already happened,
                    // deliver it now.
                    let lock_res = self.with_state(|state| {
                        match &mut state.reader_waker {
                            Some(waker) => waker.clone_from(context.waker()),
                            None => state.reader_waker = Some(context.waker().clone()),
                        }
                        Ok(state.editor.is_readable())
                    });
                    match lock_res {
                        Ok(true) => continue,
                        Ok(false) => return Poll::Pending,
                        Err(e) => return Poll::Ready(Err(e)),
                    }
                }
                Poll::Ready(Err(e)) => return Poll::Ready(Err(e)),
                Poll::Ready(Ok(0)) => {
                    // End-of-stream from raw input device.
                    let mut final_data = None;
                    let _ = self.with_state(|state| {
                        if state.editor.has_completed_bytes() {
                            let n = state.editor.pop_completed(buffer);
                            final_data = Some(Ok(n));
                        } else {
                            final_data = Some(Ok(0));
                        }
                        Ok(())
                    });
                    return Poll::Ready(final_data.unwrap_or(Ok(0)));
                }
                Poll::Ready(Ok(n)) => {
                    // Feed raw bytes into the line editor (up to the end of the
                    // first line in canonical mode) and emit echo to inner_out.
                    let feed_res = self.with_state(|state| {
                        state.pending_input.append(&raw_buf[..n]);
                        self.feed_pending_input(state);
                        Ok(())
                    });

                    if let Err(e) = feed_res {
                        return Poll::Ready(Err(e));
                    }

                    // Loop again:
                    // If the chunk completed a line, the next iteration returns it immediately.
                    // If the chunk did not complete a line, we MUST poll inner_in again until it
                    // returns Pending, ensuring context.waker() is registered before we return Pending.
                }
            }
        }
    }

    fn write(
        &self,
        _context: &mut TaskContext<'_>,
        _buffer: &[u8],
        _absolute_position: Size,
    ) -> Poll<Result<usize>> {
        Poll::Ready(Err(Error::UnsupportedOperation))
    }

    fn flush(&self, context: &mut TaskContext<'_>) -> Poll<Result<()>> {
        self.inner_in.flush(context)
    }

    fn control(
        &self,
        context: &mut TaskContext<'_>,
        command: ControlCommandIdentifier,
        input: &AnyByLayout,
        output: &mut AnyByLayout,
    ) -> Poll<Result<()>> {
        match command {
            GET_TERMINAL_ATTRIBUTES::IDENTIFIER => file_system::ready(|| {
                let attributes = GET_TERMINAL_ATTRIBUTES::cast_output(output)?;
                *attributes = self.with_state(|state| Ok(state.editor.attributes()))?;
                Ok(())
            }),
            SET_TERMINAL_ATTRIBUTES::IDENTIFIER => file_system::ready(|| {
                let attributes = *SET_TERMINAL_ATTRIBUTES::cast_input(input)?;
                if !attributes.has_only_known_flags() {
                    return Err(Error::InvalidParameter);
                }
                let reader_waker = self.with_state(|state| {
                    state.editor.set_attributes(attributes);
                    Ok(if state.editor.is_readable() {
                        state.reader_waker.take()
                    } else {
                        None
                    })
                })?;
                // Leaving canonical mode can release a partial line to a waiting reader.
                if let Some(waker) = reader_waker {
                    waker.wake();
                }
                Ok(())
            }),
            _ => self.inner_in.control(context, command, input, output),
        }
    }

    fn cancel_operation(&self, operation: Operation) {
        self.inner_in.cancel_operation(operation);
    }

    fn open(&self) -> Result<()> {
        self.inner_in.open()?;
        self.inner_out.open()?;
        Ok(())
    }

    fn close(&self) -> Result<()> {
        let in_res = self.inner_in.close();
        let out_res = self.inner_out.close();
        in_res.and(out_res)
    }
}

impl<I: DirectCharacterDevice, O: DirectCharacterDevice, const N: usize, const Q: usize>
    MountOperations for TerminalInputDevice<I, O, N, Q>
{
    fn mount(&self) -> Result<()> {
        self.inner_in.mount()?;
        self.inner_out.mount()?;
        Ok(())
    }

    fn unmount(&self) -> Result<()> {
        let in_res = self.inner_in.unmount();
        let out_res = self.inner_out.unmount();
        in_res.and(out_res)
    }
}

impl<I: DirectCharacterDevice, O: DirectCharacterDevice, const N: usize, const Q: usize>
    DirectCharacterDevice for TerminalInputDevice<I, O, N, Q>
{
}
