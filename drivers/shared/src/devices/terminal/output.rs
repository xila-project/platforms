//! VFS character device translating program output for a raw serial console.
//!
//! Programs end lines with `\n` alone, while a serial terminal needs `\r\n` to
//! return to the first column. Like the `ONLCR` output flag of a POSIX
//! terminal, this device writes every `\n` as `\r\n`.

use core::task::{Context as TaskContext, Poll};
use file_system::{
    ControlCommandIdentifier, DirectBaseOperations, DirectCharacterDevice, MountOperations,
    Operation, Result, Size,
};
use shared::AnyByLayout;

/// VFS character device writing to a raw output device with `\n` sent as `\r\n`.
///
/// `O` is the raw character output device (e.g. `StandardOutDevice`).
pub struct TerminalOutputDevice<O> {
    inner: O,
}

impl<O> TerminalOutputDevice<O> {
    /// Create a new `TerminalOutputDevice` wrapping a raw output device.
    pub const fn new(inner: O) -> Self {
        Self { inner }
    }

    /// Access the underlying raw output device.
    pub const fn inner(&self) -> &O {
        &self.inner
    }
}

impl<O: DirectCharacterDevice> DirectBaseOperations for TerminalOutputDevice<O> {
    fn read(
        &self,
        cx: &mut TaskContext<'_>,
        buffer: &mut [u8],
        absolute_position: Size,
    ) -> Poll<Result<usize>> {
        self.inner.read(cx, buffer, absolute_position)
    }

    /// Write `buffer`, sending each `\n` as `\r\n`.
    ///
    /// Returns how many bytes of `buffer` were consumed. A line feed counts as
    /// consumed only once both translated bytes were accepted; when the inner
    /// device accepts the `\r` alone, the retry sends `\r\n` again, and the
    /// extra carriage return is harmless.
    fn write(
        &self,
        cx: &mut TaskContext<'_>,
        buffer: &[u8],
        absolute_position: Size,
    ) -> Poll<Result<usize>> {
        let mut consumed = 0;
        while consumed < buffer.len() {
            let remaining = &buffer[consumed..];
            let segment_length = remaining
                .iter()
                .position(|&byte| byte == b'\n')
                .unwrap_or(remaining.len());

            let (segment, translated): (&[u8], bool) = if segment_length == 0 {
                (b"\r\n", true)
            } else {
                (&remaining[..segment_length], false)
            };

            match self.inner.write(cx, segment, absolute_position) {
                Poll::Ready(Ok(written)) => {
                    if translated {
                        if written < segment.len() {
                            break;
                        }
                        consumed += 1;
                    } else {
                        consumed += written;
                        if written < segment.len() {
                            break;
                        }
                    }
                }
                Poll::Ready(Err(error)) => {
                    if consumed == 0 {
                        return Poll::Ready(Err(error));
                    }
                    break;
                }
                Poll::Pending => {
                    if consumed == 0 {
                        return Poll::Pending;
                    }
                    break;
                }
            }
        }
        Poll::Ready(Ok(consumed))
    }

    fn flush(&self, cx: &mut TaskContext<'_>) -> Poll<Result<()>> {
        self.inner.flush(cx)
    }

    fn control(
        &self,
        cx: &mut TaskContext<'_>,
        command: ControlCommandIdentifier,
        input: &AnyByLayout,
        output: &mut AnyByLayout,
    ) -> Poll<Result<()>> {
        self.inner.control(cx, command, input, output)
    }

    fn cancel_operation(&self, operation: Operation) {
        self.inner.cancel_operation(operation);
    }

    fn open(&self) -> Result<()> {
        self.inner.open()
    }

    fn close(&self) -> Result<()> {
        self.inner.close()
    }
}

impl<O: DirectCharacterDevice> MountOperations for TerminalOutputDevice<O> {
    fn mount(&self) -> Result<()> {
        self.inner.mount()
    }

    fn unmount(&self) -> Result<()> {
        self.inner.unmount()
    }
}

impl<O: DirectCharacterDevice> DirectCharacterDevice for TerminalOutputDevice<O> {}
