use crate::io::map_error;
use file_system::{
    ControlCommand, ControlCommandIdentifier, DirectBaseOperations, DirectCharacterDevice,
    MountOperations, Size, character_device::IS_A_TERMINAL,
};
use shared::AnyByLayout;
use std::{
    collections::VecDeque,
    io::{self, Read, Write, stderr, stdin, stdout},
    sync::{Condvar, Mutex, OnceLock},
    task::Waker,
};

/// Host standard input bytes buffered ahead of Xila readers.
const INPUT_CAPACITY: usize = 4096;

#[derive(Default)]
struct Input {
    buffer: VecDeque<u8>,
    end: bool,
    error: Option<io::ErrorKind>,
    waker: Option<Waker>,
}

/// Blocking host reads happen on a dedicated thread so a reader waiting for a
/// line never stalls the executor thread (and every other task with it), e.g.
/// while a shutdown request is being processed.
struct InputReader {
    input: Mutex<Input>,
    space: Condvar,
}

impl InputReader {
    fn get() -> &'static Self {
        static READER: OnceLock<&'static InputReader> = OnceLock::new();
        READER.get_or_init(|| {
            let reader: &'static Self = Box::leak(Box::new(Self {
                input: Mutex::new(Input::default()),
                space: Condvar::new(),
            }));
            if let Err(error) = std::thread::Builder::new()
                .name("xila-stdin".into())
                .spawn(move || reader.run())
            {
                reader.lock().error = Some(error.kind());
            }
            reader
        })
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Input> {
        self.input
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    fn run(&self) {
        let mut chunk = [0u8; 256];
        loop {
            {
                let mut input = self.lock();
                while input.buffer.len() >= INPUT_CAPACITY {
                    input = self
                        .space
                        .wait(input)
                        .unwrap_or_else(|poisoned| poisoned.into_inner());
                }
            }
            let result = stdin().read(&mut chunk);
            let mut input = self.lock();
            match result {
                Ok(0) => input.end = true,
                Ok(count) => input.buffer.extend(&chunk[..count]),
                Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
                Err(error) => input.error = Some(error.kind()),
            }
            let finished = input.end || input.error.is_some();
            if let Some(waker) = input.waker.take() {
                waker.wake();
            }
            if finished {
                return;
            }
        }
    }

    fn poll_read(
        &self,
        cx: &mut core::task::Context<'_>,
        buffer: &mut [u8],
    ) -> core::task::Poll<file_system::Result<usize>> {
        if buffer.is_empty() {
            return core::task::Poll::Ready(Ok(0));
        }
        let mut input = self.lock();
        if !input.buffer.is_empty() {
            let count = buffer.len().min(input.buffer.len());
            for (target, byte) in buffer.iter_mut().zip(input.buffer.drain(..count)) {
                *target = byte;
            }
            self.space.notify_one();
            return core::task::Poll::Ready(Ok(count));
        }
        if let Some(kind) = input.error {
            return core::task::Poll::Ready(Err(map_error(kind.into())));
        }
        if input.end {
            return core::task::Poll::Ready(Ok(0));
        }
        input.waker = Some(cx.waker().clone());
        core::task::Poll::Pending
    }
}

pub struct StandardInDevice;

impl DirectBaseOperations for StandardInDevice {
    /// Returns `Pending` (registering the waker) until host input arrives;
    /// synchronous adapters therefore see `RessourceBusy` instead of blocking.
    fn read(
        &self,
        cx: &mut core::task::Context<'_>,
        buffer: &mut [u8],
        _: Size,
    ) -> core::task::Poll<file_system::Result<usize>> {
        InputReader::get().poll_read(cx, buffer)
    }

    fn write(
        &self,
        _cx: &mut core::task::Context<'_>,
        _: &[u8],
        _: Size,
    ) -> core::task::Poll<file_system::Result<usize>> {
        file_system::ready(|| Err(file_system::Error::UnsupportedOperation))
    }

    fn flush(
        &self,
        _cx: &mut core::task::Context<'_>,
    ) -> core::task::Poll<file_system::Result<()>> {
        file_system::ready(|| Ok(()))
    }

    fn control(
        &self,
        _cx: &mut core::task::Context<'_>,
        command: ControlCommandIdentifier,
        input: &AnyByLayout,
        output: &mut AnyByLayout,
    ) -> core::task::Poll<file_system::Result<()>> {
        file_system::ready(|| control(command, input, output))
    }
}

impl MountOperations for StandardInDevice {}

impl DirectCharacterDevice for StandardInDevice {}

pub struct StandardOutDevice;

impl DirectBaseOperations for StandardOutDevice {
    fn read(
        &self,
        _cx: &mut core::task::Context<'_>,
        _: &mut [u8],
        _: Size,
    ) -> core::task::Poll<file_system::Result<usize>> {
        file_system::ready(|| Err(file_system::Error::UnsupportedOperation))
    }

    fn write(
        &self,
        _cx: &mut core::task::Context<'_>,
        buffer: &[u8],
        _: Size,
    ) -> core::task::Poll<file_system::Result<usize>> {
        file_system::ready(|| Ok(stdout().write(buffer).map_err(map_error)? as _))
    }

    fn flush(
        &self,
        _cx: &mut core::task::Context<'_>,
    ) -> core::task::Poll<file_system::Result<()>> {
        file_system::ready(|| stdout().flush().map_err(map_error))
    }

    fn control(
        &self,
        _cx: &mut core::task::Context<'_>,
        command: ControlCommandIdentifier,
        input: &AnyByLayout,
        output: &mut AnyByLayout,
    ) -> core::task::Poll<file_system::Result<()>> {
        file_system::ready(|| control(command, input, output))
    }
}

impl MountOperations for StandardOutDevice {}

impl DirectCharacterDevice for StandardOutDevice {}

pub struct StandardErrorDevice;

impl DirectBaseOperations for StandardErrorDevice {
    fn read(
        &self,
        _cx: &mut core::task::Context<'_>,
        _: &mut [u8],
        _: Size,
    ) -> core::task::Poll<file_system::Result<usize>> {
        file_system::ready(|| Err(file_system::Error::UnsupportedOperation))
    }

    fn write(
        &self,
        _cx: &mut core::task::Context<'_>,
        buffer: &[u8],
        _: Size,
    ) -> core::task::Poll<file_system::Result<usize>> {
        file_system::ready(|| Ok(stderr().write(buffer).map_err(map_error)? as _))
    }

    fn flush(
        &self,
        _cx: &mut core::task::Context<'_>,
    ) -> core::task::Poll<file_system::Result<()>> {
        file_system::ready(|| stderr().flush().map_err(map_error))
    }

    fn control(
        &self,
        _cx: &mut core::task::Context<'_>,
        command: ControlCommandIdentifier,
        input: &AnyByLayout,
        output: &mut AnyByLayout,
    ) -> core::task::Poll<file_system::Result<()>> {
        file_system::ready(|| control(command, input, output))
    }
}

impl MountOperations for StandardErrorDevice {}

impl DirectCharacterDevice for StandardErrorDevice {}

fn control(
    command: ControlCommandIdentifier,
    _: &AnyByLayout,
    output: &mut AnyByLayout,
) -> file_system::Result<()> {
    match command {
        IS_A_TERMINAL::IDENTIFIER => {
            let output: &mut bool = IS_A_TERMINAL::cast_output(output)?;
            *output = true;

            Ok(())
        }
        _ => Err(file_system::Error::UnsupportedOperation),
    }
}
