//! Asynchronous serial console using esp-hal's interrupt-driven transmit and
//! receive halves. The peripheral adapter owns a blocking driver and cannot
//! register a waker or wait asynchronously for transmission to complete.
//!
//! The console runs over a UART or, on chips that have one, over the built-in
//! USB Serial/JTAG controller, for boards whose USB port has no USB to UART
//! bridge.
//!
//! The logger shares the console with the interactive session. Call
//! [`set_interactive`] once a session can read the console: from then on,
//! informational messages are left out and warnings and errors are rate limited
//! (see [`LogGate`]), and a message always starts on its own line.

use core::{
    cell::{Cell, RefCell},
    fmt,
    future::poll_fn,
    task::{Context, Poll},
};

use critical_section::Mutex;
use drivers_shared::devices::terminal::{
    EchoBuffer, EchoSink, LogAdmission, LogGate, LogImportance, RingBuffer, TerminalInputDevice,
    TerminalOutputDevice,
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
/// Room for the whole boot report: a USB Serial/JTAG console only transmits
/// once the host opens the port, which can be seconds after a reset.
const OUTPUT_CAPACITY: usize = 4096;
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
    /// Whether the last queued byte ended a line.
    at_line_start: bool,
    log_gate: LogGate,
}

impl OutputState {
    fn push(&mut self, bytes: &[u8]) -> usize {
        let written = self.queue.push_slice(bytes);
        if written != 0 {
            self.at_line_start = matches!(bytes[written - 1], b'\n' | b'\r');
        }
        written
    }
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
    at_line_start: true,
    log_gate: LogGate::new(),
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
            let written = output.push(buffer);
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

fn monotonic_now() -> core::time::Duration {
    core::time::Duration::from_micros(Instant::now().duration_since_epoch().as_micros())
}

/// Tells the logger whether an interactive session uses the console. See the
/// module documentation.
pub fn set_interactive(interactive: bool) {
    let now = monotonic_now();
    with_output(|output| output.log_gate.set_interactive(interactive, now));
}

fn importance(level: log::Level) -> LogImportance {
    match level {
        log::Level::Error | log::Level::Warning => LogImportance::Problem,
        _ => LogImportance::Information,
    }
}

/// Queues `bytes` and a line ending when they fit entirely, starting on a new
/// line when the console is in the middle of one.
fn push_line(output: &mut OutputState, bytes: &[u8]) -> bool {
    let separator: &[u8] = if output.at_line_start { b"" } else { b"\r\n" };
    if output.queue.available_space() < separator.len() + bytes.len() + 2 {
        return false;
    }
    output.push(separator);
    output.push(bytes);
    output.push(b"\r\n");
    true
}

impl Logger {
    fn emit(&self, importance: LogImportance, arguments: fmt::Arguments<'_>) {
        let now = monotonic_now();
        let admission = with_output(|output| output.log_gate.admit(importance, now));
        let LogAdmission::Write { dropped } = admission else {
            return;
        };
        let mut message = LogMessage(EchoBuffer::new());
        let _ = fmt::write(&mut message, arguments);
        let mut note = LogMessage(EchoBuffer::new());
        if dropped != 0 {
            let _ = fmt::write(
                &mut note,
                format_args!("({dropped} log messages were dropped)"),
            );
        }
        with_output(|output| {
            // A full queue drops the message, and the next message that fits
            // reports it, like the messages the rate limit drops.
            let note_written = dropped == 0 || push_line(output, note.0.as_bytes());
            let message_written = note_written && push_line(output, message.0.as_bytes());
            if (dropped != 0 && note_written) || message_written {
                output.runner.wake();
            }
            let lost = if note_written { 0 } else { dropped } + u32::from(!message_written);
            if lost != 0 {
                output.log_gate.record_dropped(lost);
            }
        });
    }
}

impl log::LoggerTrait for Logger {
    fn enabled(&self, level: log::Level) -> bool {
        matches!(
            level,
            log::Level::Error | log::Level::Warning | log::Level::Information
        )
    }

    fn write(&self, arguments: fmt::Arguments<'_>) {
        self.emit(LogImportance::Information, arguments);
    }

    fn log(&self, record: &log::Record<'_>) {
        // The same layout as Core's default, which this override replaces to
        // learn the level of the message.
        let (letter, color) = match record.level {
            log::Level::Error => ("E", "\x1b[0;41m"),
            log::Level::Warning => ("W", "\x1b[0;43m"),
            log::Level::Information => ("I", "\x1b[0;46m"),
            log::Level::Debug => ("D", "\x1b[0;42m"),
            log::Level::Trace => ("T", "\x1b[0;100m"),
        };
        self.emit(
            importance(record.level),
            format_args!(
                "{color} {letter} \x1b[0m | \x1b[0;1m{}\x1b[0m | {}",
                record.target, record.arguments
            ),
        );
    }
}

/// Queues received bytes for the readers. Returns whether some did not fit.
fn accept_input(bytes: &[u8]) -> bool {
    with_input(|input| {
        // Preserve bytes already queued and drop any new bytes that do not fit.
        let accepted = input.queue.push_slice(bytes);
        if accepted != 0 {
            input.readers.wake();
        }
        accepted < bytes.len()
    })
}

/// Warns about lost input at most once a second.
fn report_overflow(last_warning: &mut Option<Instant>) {
    let now = Instant::now();
    if last_warning
        .is_none_or(|previous| now < previous || now - previous >= Duration::from_secs(1))
    {
        *last_warning = Some(now);
        log::warning!("Serial console input overflowed and dropped bytes");
    }
}

/// Waits for queued output and moves it to `buffer`.
async fn next_output(buffer: &mut [u8]) -> usize {
    poll_fn(|context| {
        with_output(|output| {
            let count = output.queue.pop_slice(buffer);
            if count == 0 {
                output.runner.register(context.waker());
                return Poll::Pending;
            }
            output.transmitting = true;
            output.writers.wake();
            Poll::Ready(count)
        })
    })
    .await
}

fn output_is_empty() -> bool {
    with_output(|output| output.queue.is_empty())
}

/// Whether everything queued so far has been sent to the port.
pub fn is_output_idle() -> bool {
    with_output(|output| output.queue.is_empty() && !output.transmitting)
}

fn finish_transmission() {
    with_output(|output| {
        output.transmitting = false;
        output.writers.wake();
    });
}

#[cfg(any(
    feature = "esp32c3",
    feature = "esp32c5",
    feature = "esp32c6",
    feature = "esp32c61",
    feature = "esp32h2",
    feature = "esp32p4",
    feature = "esp32s3",
    feature = "esp32s31",
))]
mod usb_serial_jtag {
    use embedded_io_async::{Read, Write};
    use esp_hal::{
        Async,
        usb::usb_serial_jtag::{UsbSerialJtagRx, UsbSerialJtagTx},
    };

    use super::{
        TRANSFER_SIZE, accept_input, finish_transmission, next_output, output_is_empty,
        report_overflow,
    };

    pub(super) async fn receive_bytes(mut receive: UsbSerialJtagRx<'static, Async>) -> ! {
        let mut buffer = [0; TRANSFER_SIZE];
        let mut last_warning = None;
        loop {
            // Reading the controller's FIFO cannot fail.
            let count = receive.read(&mut buffer).await.unwrap_or(0);
            if accept_input(&buffer[..count]) {
                report_overflow(&mut last_warning);
            }
        }
    }

    /// Without a host reading the port, a write waits until one opens it: the
    /// queued output then waits too, and the logger drops what does not fit.
    pub(super) async fn transmit_bytes(mut transmit: UsbSerialJtagTx<'static, Async>) -> ! {
        let mut buffer = [0; TRANSFER_SIZE];
        loop {
            let count = next_output(&mut buffer).await;
            // Writing to the controller's FIFO cannot fail.
            let _ = transmit.write_all(&buffer[..count]).await;
            if !output_is_empty() {
                continue;
            }
            // Send the packet now rather than when the FIFO fills.
            let _ = transmit.flush().await;
            finish_transmission();
        }
    }
}

enum Transport {
    Uart(Uart<'static, Async>),
    #[cfg(any(
        feature = "esp32c3",
        feature = "esp32c5",
        feature = "esp32c6",
        feature = "esp32c61",
        feature = "esp32h2",
        feature = "esp32p4",
        feature = "esp32s3",
        feature = "esp32s31",
    ))]
    UsbSerialJtag(esp_hal::usb::usb_serial_jtag::UsbSerialJtag<'static, Async>),
}

/// A configured console port, before the console task takes it. Unlike the
/// runner, it can be handed between contexts before the executor starts.
pub enum ConsolePort {
    /// A UART.
    Uart(Uart<'static, Blocking>),
    /// The chip's USB Serial/JTAG controller, for boards whose USB port has no
    /// USB to UART bridge.
    #[cfg(any(
        feature = "esp32c3",
        feature = "esp32c5",
        feature = "esp32c6",
        feature = "esp32c61",
        feature = "esp32h2",
        feature = "esp32p4",
        feature = "esp32s3",
        feature = "esp32s31",
    ))]
    UsbSerialJtag(esp_hal::usb::usb_serial_jtag::UsbSerialJtag<'static, Blocking>),
}

impl From<Uart<'static, Blocking>> for ConsolePort {
    fn from(serial: Uart<'static, Blocking>) -> Self {
        Self::Uart(serial)
    }
}

#[cfg(any(
    feature = "esp32c3",
    feature = "esp32c5",
    feature = "esp32c6",
    feature = "esp32c61",
    feature = "esp32h2",
    feature = "esp32p4",
    feature = "esp32s3",
    feature = "esp32s31",
))]
impl From<esp_hal::usb::usb_serial_jtag::UsbSerialJtag<'static, Blocking>> for ConsolePort {
    fn from(controller: esp_hal::usb::usb_serial_jtag::UsbSerialJtag<'static, Blocking>) -> Self {
        Self::UsbSerialJtag(controller)
    }
}

/// Owns the console port for the duration of the console task.
pub struct ConsoleRunner(Transport);

fn claim_console() {
    critical_section::with(|section| {
        assert!(
            !INITIALIZED.borrow(section).replace(true),
            "the serial console is already initialized"
        );
    });
}

impl ConsoleRunner {
    /// Takes ownership of an already configured console port: a UART, or the
    /// USB Serial/JTAG controller on the chips that have one.
    ///
    /// Call once, on the core that will run the console.
    pub fn new(port: impl Into<ConsolePort>) -> Self {
        claim_console();
        Self(match port.into() {
            ConsolePort::Uart(serial) => Transport::Uart(serial.into_async()),
            #[cfg(any(
                feature = "esp32c3",
                feature = "esp32c5",
                feature = "esp32c6",
                feature = "esp32c61",
                feature = "esp32h2",
                feature = "esp32p4",
                feature = "esp32s3",
                feature = "esp32s31",
            ))]
            ConsolePort::UsbSerialJtag(controller) => {
                Transport::UsbSerialJtag(controller.into_async())
            }
        })
    }

    /// Pumps input and output until the firmware stops.
    pub async fn run(self) -> ! {
        match self.0 {
            Transport::Uart(serial) => {
                let (receive, transmit) = serial.split();
                let (never, _) = join(receive_bytes(receive), transmit_bytes(transmit)).await;
                never
            }
            #[cfg(any(
                feature = "esp32c3",
                feature = "esp32c5",
                feature = "esp32c6",
                feature = "esp32c61",
                feature = "esp32h2",
                feature = "esp32p4",
                feature = "esp32s3",
                feature = "esp32s31",
            ))]
            Transport::UsbSerialJtag(controller) => {
                let (receive, transmit) = controller.split();
                let (never, _) = join(
                    usb_serial_jtag::receive_bytes(receive),
                    usb_serial_jtag::transmit_bytes(transmit),
                )
                .await;
                never
            }
        }
    }
}

async fn receive_bytes(mut receive: UartRx<'static, Async>) -> ! {
    let mut buffer = [0; TRANSFER_SIZE];
    let mut last_warning = None;
    loop {
        let overflow = match receive.read_async(&mut buffer).await {
            Ok(count) => accept_input(&buffer[..count]),
            Err(RxError::FifoOverflowed) => true,
            Err(_) => false,
        };

        if overflow {
            report_overflow(&mut last_warning);
        }
    }
}

async fn transmit_bytes(mut transmit: UartTx<'static, Async>) -> ! {
    let mut buffer = [0; TRANSFER_SIZE];
    loop {
        let count = next_output(&mut buffer).await;

        let mut remaining = &buffer[..count];
        while !remaining.is_empty() {
            let written = transmit
                .write_async(remaining)
                .await
                .expect("serial write failed");
            remaining = &remaining[written..];
        }

        // Keep the transmit FIFO fed while more output is queued; wait for the
        // line to go idle only once the queue is empty.
        if !output_is_empty() {
            continue;
        }
        transmit.flush_async().await.expect("serial flush failed");

        finish_transmission();
    }
}
