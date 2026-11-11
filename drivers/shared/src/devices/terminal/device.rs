//! VFS character device wrapping a raw serial console (Layer 2).
//!
//! Provides canonical mode cooked input for the Xila shell and applications,
//! translating raw input into line-buffered input with echo.

use core::cell::RefCell;
use core::task::{Context as TaskContext, Poll, Waker};
use critical_section::Mutex;
use file_system::{
    ControlCommandIdentifier, DirectBaseOperations, DirectCharacterDevice, Error, MountOperations,
    Operation, Result, Size,
};
use shared::AnyByLayout;

use super::logic::LineEditor;

/// VFS character device providing cooked terminal input over raw I/O devices.
///
/// `I` is the raw character input device (e.g. `StandardInDevice`).
/// `O` is the raw character output device for terminal echo (e.g. `StandardOutDevice`).
/// `N` is the maximum line capacity in bytes (default 256).
/// `Q` is the type-ahead completed line queue capacity in bytes (default 512).
pub struct TerminalInputDevice<I, O, const N: usize = 256, const Q: usize = 512> {
    inner_in: I,
    inner_out: O,
    state: Mutex<RefCell<LineEditor<N, Q>>>,
}

impl<I, O, const N: usize, const Q: usize> TerminalInputDevice<I, O, N, Q> {
    /// Create a new `TerminalInputDevice` wrapping raw input and output devices with custom capacities.
    pub const fn new_with_capacities(inner_in: I, inner_out: O) -> Self {
        Self {
            inner_in,
            inner_out,
            state: Mutex::new(RefCell::new(LineEditor::new())),
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
    fn with_state<R>(&self, f: impl FnOnce(&mut LineEditor<N, Q>) -> Result<R>) -> Result<R> {
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
        cx: &mut TaskContext<'_>,
        buffer: &mut [u8],
        _absolute_position: Size,
    ) -> Poll<Result<usize>> {
        if buffer.is_empty() {
            return Poll::Ready(Ok(0));
        }

        loop {
            // 1. Deliver completed lines or EOF if already available in the editor.
            let mut data_to_return = None;
            let lock_res = self.with_state(|state| {
                if state.has_completed_bytes() {
                    let n = state.pop_completed(buffer);
                    data_to_return = Some(Ok(n));
                } else if state.take_eof() {
                    data_to_return = Some(Ok(0));
                }
                Ok(())
            });

            if let Err(e) = lock_res {
                return Poll::Ready(Err(e));
            }

            if let Some(res) = data_to_return {
                return Poll::Ready(res);
            }

            // 2. No completed data available: pull raw bytes from the inner input device.
            let mut raw_buf = [0u8; 64];
            match self.inner_in.read(cx, &mut raw_buf, 0) {
                Poll::Pending => {
                    // Inner device has registered the waker from cx.
                    // Returning Pending guarantees a wake-up when hardware input arrives.
                    return Poll::Pending;
                }
                Poll::Ready(Err(e)) => return Poll::Ready(Err(e)),
                Poll::Ready(Ok(0)) => {
                    // End-of-stream from raw input device.
                    let mut final_data = None;
                    let _ = self.with_state(|state| {
                        if state.has_completed_bytes() {
                            let n = state.pop_completed(buffer);
                            final_data = Some(Ok(n));
                        } else {
                            final_data = Some(Ok(0));
                        }
                        Ok(())
                    });
                    return Poll::Ready(final_data.unwrap_or(Ok(0)));
                }
                Poll::Ready(Ok(n)) => {
                    // Feed raw bytes into line editor and emit echo to inner_out.
                    let feed_res = self.with_state(|state| {
                        let mut echo_fn = |echo_bytes: &[u8]| {
                            let mut noop_cx = TaskContext::from_waker(Waker::noop());
                            let _ = self.inner_out.write(&mut noop_cx, echo_bytes, 0);
                        };
                        state.feed_slice(&raw_buf[..n], &mut echo_fn);
                        Ok(())
                    });

                    if let Err(e) = feed_res {
                        return Poll::Ready(Err(e));
                    }

                    // Loop again:
                    // If the chunk completed a line, the next iteration returns it immediately.
                    // If the chunk did not complete a line, we MUST poll inner_in again until it
                    // returns Pending, ensuring cx.waker() is registered before we return Pending.
                }
            }
        }
    }

    fn write(
        &self,
        _cx: &mut TaskContext<'_>,
        _buffer: &[u8],
        _absolute_position: Size,
    ) -> Poll<Result<usize>> {
        Poll::Ready(Err(Error::UnsupportedOperation))
    }

    fn flush(&self, cx: &mut TaskContext<'_>) -> Poll<Result<()>> {
        self.inner_in.flush(cx)
    }

    fn control(
        &self,
        cx: &mut TaskContext<'_>,
        command: ControlCommandIdentifier,
        input: &AnyByLayout,
        output: &mut AnyByLayout,
    ) -> Poll<Result<()>> {
        self.inner_in.control(cx, command, input, output)
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
