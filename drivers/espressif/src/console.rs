//! Asynchronous serial console using the HAL's interrupt-driven transmit and
//! receive halves. The peripheral adapter owns a blocking driver and cannot
//! register a waker or wait asynchronously for transmission to complete.

use core::{
    cell::{Cell, RefCell},
    fmt,
    future::poll_fn,
    task::{Context, Poll},
};

use critical_section::Mutex;
use drivers_shared::devices::terminal::{
    EchoBuffer, EchoSink, RingBuffer, TerminalInputDevice, TerminalOutputDevice,
};
use embassy_futures::join::join;
use embassy_sync::waitqueue::{MultiWakerRegistration, WakerRegistration};
use esp_hal::{
    Async, Blocking,
    time::{Duration, Instant},
    uart::{RxError, Uart, UartRx, UartTx},
};
use file_system::{
    ControlCommand, ControlCommandIdentifier, DirectBaseOperations, DirectCharacterDevice,
    IoErrorKind, MountOperations, Operation, Result, Size, character_device::IS_A_TERMINAL,
};
use shared::AnyByLayout;

const INPUT_CAPACITY: usize = 256;
const OUTPUT_CAPACITY: usize = 1024;
const TRANSFER_SIZE: usize = 64;

struct InputState {
    queue: RingBuffer<INPUT_CAPACITY>,
    readers: MultiWakerRegistration<8>,
}

struct OutputState {
    queue: RingBuffer<OUTPUT_CAPACITY>,
    writers: MultiWakerRegistration<8>,
    runner: WakerRegistration,
    transmitting: bool,
}

static INITIALIZED: Mutex<Cell<bool>> = Mutex::new(Cell::new(false));
static INPUT: Mutex<RefCell<InputState>> = Mutex::new(RefCell::new(InputState {
    queue: RingBuffer::new(),
    readers: MultiWakerRegistration::new(),
}));
static OUTPUT: Mutex<RefCell<OutputState>> = Mutex::new(RefCell::new(OutputState {
    queue: RingBuffer::new(),
    writers: MultiWakerRegistration::new(),
    runner: WakerRegistration::new(),
    transmitting: false,
}));

fn with_input<T>(function: impl FnOnce(&mut InputState) -> T) -> T {
    critical_section::with(|section| function(&mut INPUT.borrow(section).borrow_mut()))
}

fn with_output<T>(function: impl FnOnce(&mut OutputState) -> T) -> T {
    critical_section::with(|section| function(&mut OUTPUT.borrow(section).borrow_mut()))
}

fn terminal_control(
    command: ControlCommandIdentifier,
    output: &mut AnyByLayout,
) -> Poll<Result<()>> {
    match command {
        IS_A_TERMINAL::IDENTIFIER => {
            *IS_A_TERMINAL::cast_output(output)? = true;
            Poll::Ready(Ok(()))
        }
        _ => Poll::Ready(Err(IoErrorKind::Unsupported)),
    }
}

/// Raw standard input from the serial port.
#[derive(Clone, Copy, Default)]
pub struct StandardInputDevice;

impl DirectBaseOperations for StandardInputDevice {
    fn read(&self, context: &mut Context<'_>, buffer: &mut [u8], _: Size) -> Poll<Result<usize>> {
        if buffer.is_empty() {
            return Poll::Ready(Ok(0));
        }

        with_input(|input| {
            if input.queue.is_empty() {
                input.readers.register(context.waker());
                return Poll::Pending;
            }
            Poll::Ready(Ok(input.queue.pop_slice(buffer)))
        })
    }

    fn write(&self, _: &mut Context<'_>, _: &[u8], _: Size) -> Poll<Result<usize>> {
        Poll::Ready(Err(IoErrorKind::Unsupported))
    }

    fn flush(&self, _: &mut Context<'_>) -> Poll<Result<()>> {
        Poll::Ready(Ok(()))
    }

    fn control(
        &self,
        _: &mut Context<'_>,
        command: ControlCommandIdentifier,
        _: &AnyByLayout,
        output: &mut AnyByLayout,
    ) -> Poll<Result<()>> {
        terminal_control(command, output)
    }

    fn cancel_operation(&self, operation: Operation) {
        if operation == Operation::Read {
            with_input(|input| input.readers.wake());
        }
    }
}

impl MountOperations for StandardInputDevice {}
impl DirectCharacterDevice for StandardInputDevice {}

/// Raw standard output and error to the serial port.
#[derive(Clone, Copy, Default)]
pub struct StandardOutputDevice;

impl DirectBaseOperations for StandardOutputDevice {
    fn read(&self, _: &mut Context<'_>, _: &mut [u8], _: Size) -> Poll<Result<usize>> {
        Poll::Ready(Err(IoErrorKind::Unsupported))
    }

    fn write(&self, context: &mut Context<'_>, buffer: &[u8], _: Size) -> Poll<Result<usize>> {
        if buffer.is_empty() {
            return Poll::Ready(Ok(0));
        }

        with_output(|output| {
            // Give the shared output device room to write a translated
            // carriage return and line feed together.
            if output.queue.available_space() < buffer.len().min(2) {
                output.writers.register(context.waker());
                return Poll::Pending;
            }
            let written = output.queue.push_slice(buffer);
            output.runner.wake();
            Poll::Ready(Ok(written))
        })
    }

    fn flush(&self, context: &mut Context<'_>) -> Poll<Result<()>> {
        with_output(|output| {
            if output.queue.is_empty() && !output.transmitting {
                Poll::Ready(Ok(()))
            } else {
                output.writers.register(context.waker());
                Poll::Pending
            }
        })
    }

    fn control(
        &self,
        _: &mut Context<'_>,
        command: ControlCommandIdentifier,
        _: &AnyByLayout,
        output: &mut AnyByLayout,
    ) -> Poll<Result<()>> {
        terminal_control(command, output)
    }

    fn cancel_operation(&self, operation: Operation) {
        if matches!(operation, Operation::Write | Operation::Flush) {
            with_output(|output| output.writers.wake());
        }
    }
}

impl MountOperations for StandardOutputDevice {}
impl DirectCharacterDevice for StandardOutputDevice {}

/// Creates the standard input device with shared line editing and echo.
pub const fn standard_input() -> TerminalInputDevice<StandardInputDevice, StandardOutputDevice> {
    TerminalInputDevice::new(StandardInputDevice, StandardOutputDevice)
}

/// Creates the standard output device with serial line ending conversion.
pub const fn standard_output() -> TerminalOutputDevice<StandardOutputDevice> {
    TerminalOutputDevice::new(StandardOutputDevice)
}

struct LogMessage(EchoBuffer<256>);

impl fmt::Write for LogMessage {
    fn write_str(&mut self, text: &str) -> fmt::Result {
        self.0.emit(text.as_bytes());
        Ok(())
    }
}

/// Logger that queues serial output without waiting for the port.
pub struct Logger;

pub static LOGGER: Logger = Logger;

impl log::LoggerTrait for Logger {
    fn enabled(&self, level: log::Level) -> bool {
        matches!(
            level,
            log::Level::Error | log::Level::Warn | log::Level::Info
        )
    }

    fn write(&self, arguments: fmt::Arguments<'_>) {
        let mut message = LogMessage(EchoBuffer::new());
        let _ = fmt::write(&mut message, arguments);
        with_output(|output| {
            let bytes = message.0.as_bytes();
            if output.queue.available_space() >= bytes.len() + 2 {
                output.queue.push_slice(bytes);
                output.queue.push_slice(b"\r\n");
                output.runner.wake();
            }
        });
    }
}

/// Owns the serial port for the duration of the console task.
pub struct ConsoleRunner(Uart<'static, Async>);

impl ConsoleRunner {
    /// Takes ownership of an already configured serial port.
    ///
    /// Call once, on the core that will run the console.
    pub fn new(serial: Uart<'static, Blocking>) -> Self {
        critical_section::with(|section| {
            assert!(
                !INITIALIZED.borrow(section).replace(true),
                "the serial console is already initialized"
            );
        });
        Self(serial.into_async())
    }

    /// Pumps input and output until the firmware stops.
    pub async fn run(self) -> ! {
        let (receive, transmit) = self.0.split();
        let (never, _) = join(receive_bytes(receive), transmit_bytes(transmit)).await;
        never
    }
}

async fn receive_bytes(mut receive: UartRx<'static, Async>) -> ! {
    let mut buffer = [0; TRANSFER_SIZE];
    let mut last_warning = None;
    loop {
        let overflow = match receive.read_async(&mut buffer).await {
            Ok(count) => with_input(|input| {
                // Preserve bytes already queued and drop any new bytes that do not fit.
                let accepted = input.queue.push_slice(&buffer[..count]);
                if accepted != 0 {
                    input.readers.wake();
                }
                accepted < count
            }),
            Err(RxError::FifoOverflowed) => true,
            Err(_) => false,
        };

        if overflow {
            let now = Instant::now();
            if last_warning
                .is_none_or(|previous| now < previous || now - previous >= Duration::from_secs(1))
            {
                last_warning = Some(now);
                log::warning!("Serial console input overflowed and dropped bytes");
            }
        }
    }
}

async fn transmit_bytes(mut transmit: UartTx<'static, Async>) -> ! {
    let mut buffer = [0; TRANSFER_SIZE];
    loop {
        let count = poll_fn(|context| {
            with_output(|output| {
                let count = output.queue.pop_slice(&mut buffer);
                if count == 0 {
                    output.runner.register(context.waker());
                    return Poll::Pending;
                }
                output.transmitting = true;
                output.writers.wake();
                Poll::Ready(count)
            })
        })
        .await;

        let mut remaining = &buffer[..count];
        while !remaining.is_empty() {
            let written = transmit
                .write_async(remaining)
                .await
                .expect("serial write failed");
            remaining = &remaining[written..];
        }
        transmit.flush_async().await.expect("serial flush failed");

        with_output(|output| {
            output.transmitting = false;
            output.writers.wake();
        });
    }
}
