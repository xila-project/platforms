use core::cell::RefCell;
use core::future::poll_fn;
use core::pin::pin;
use core::sync::atomic::{AtomicBool, Ordering};
use core::task::Poll;
use critical_section::Mutex;
use embassy_futures::join::join3;
use embassy_futures::select::{Either, select};
use embassy_nrf::Peri;
use embassy_nrf::peripherals::USBD;
use embassy_nrf::usb::Driver;
use embassy_nrf::usb::vbus_detect::HardwareVbusDetect;
use embassy_sync::waitqueue::WakerRegistration;
use embassy_usb::class::cdc_acm::{CdcAcmClass, ControlChanged, Receiver, Sender, State};
use embassy_usb::{Builder, UsbDevice};
use file_system::character_device::IS_A_TERMINAL;
use file_system::{
    ControlCommand, ControlCommandIdentifier, DirectBaseOperations, DirectCharacterDevice, Error,
    MountOperations, Operation, Result, Size,
};
use shared::AnyByLayout;

use super::logic::{QueueWriter, RingBuffer, is_1200_bps_touch};

embassy_nrf::bind_interrupts!(struct Irqs {
    USBD => embassy_nrf::usb::InterruptHandler<embassy_nrf::peripherals::USBD>;
    CLOCK_POWER => embassy_nrf::usb::vbus_detect::InterruptHandler;
});

/// Configuration for the USB CDC-ACM serial console.
#[derive(Debug, Clone, Copy)]
pub struct UsbConsoleConfig {
    pub vendor_id: u16,
    pub product_id: u16,
    pub manufacturer: Option<&'static str>,
    pub product: Option<&'static str>,
    pub serial_number: Option<&'static str>,
    /// Optional hook called when a 1200-bps touch sequence is detected (used by Arduino boards to enter bootloader).
    pub on_1200_bps_touch: Option<fn()>,
}

impl Default for UsbConsoleConfig {
    fn default() -> Self {
        Self {
            vendor_id: 0x1209,
            product_id: 0x0001,
            manufacturer: Some("Xila"),
            product: Some("Xila USB Serial"),
            serial_number: None,
            on_1200_bps_touch: None,
        }
    }
}

const INPUT_QUEUE_CAPACITY: usize = 128;
const OUTPUT_QUEUE_CAPACITY: usize = 1024;

struct InputState {
    queue: RingBuffer<INPUT_QUEUE_CAPACITY>,
    reader_waker: WakerRegistration,
    runner_waker: WakerRegistration,
}

impl InputState {
    const fn new() -> Self {
        Self {
            queue: RingBuffer::new(),
            reader_waker: WakerRegistration::new(),
            runner_waker: WakerRegistration::new(),
        }
    }
}

struct OutputState {
    queue: RingBuffer<OUTPUT_QUEUE_CAPACITY>,
    writer_waker: WakerRegistration,
    runner_waker: WakerRegistration,
}

impl OutputState {
    const fn new() -> Self {
        Self {
            queue: RingBuffer::new(),
            writer_waker: WakerRegistration::new(),
            runner_waker: WakerRegistration::new(),
        }
    }
}

static INPUT_QUEUE: Mutex<RefCell<InputState>> = Mutex::new(RefCell::new(InputState::new()));
static OUTPUT_QUEUE: Mutex<RefCell<OutputState>> = Mutex::new(RefCell::new(OutputState::new()));
static DTR_CONNECTED: AtomicBool = AtomicBool::new(false);
static HOST_SEEN: AtomicBool = AtomicBool::new(false);
static INITIALIZED: AtomicBool = AtomicBool::new(false);

/// Whether a host terminal has opened the port since the start.
pub fn host_seen() -> bool {
    HOST_SEEN.load(Ordering::Relaxed)
}

struct UsbBuffers {
    config_descriptor: [u8; 256],
    bos_descriptor: [u8; 16],
    msos_descriptor: [u8; 16],
    control: [u8; 64],
    cdc_state: State<'static>,
}

static mut USB_BUFFERS: UsbBuffers = UsbBuffers {
    config_descriptor: [0; 256],
    bos_descriptor: [0; 16],
    msos_descriptor: [0; 16],
    control: [0; 64],
    cdc_state: State::new(),
};

/// VFS character device representing standard input from USB serial.
#[derive(Clone, Copy, Default)]
pub struct StandardInDevice;

impl DirectBaseOperations for StandardInDevice {
    fn read(
        &self,
        cx: &mut core::task::Context<'_>,
        buffer: &mut [u8],
        _absolute_position: Size,
    ) -> Poll<Result<usize>> {
        if buffer.is_empty() {
            return Poll::Ready(Ok(0));
        }

        critical_section::with(|cs| {
            let mut input = INPUT_QUEUE.borrow(cs).borrow_mut();
            if input.queue.is_empty() {
                input.reader_waker.register(cx.waker());
                Poll::Pending
            } else {
                let n = input.queue.pop_slice(buffer);
                input.runner_waker.wake();
                Poll::Ready(Ok(n))
            }
        })
    }

    fn write(
        &self,
        _cx: &mut core::task::Context<'_>,
        _buffer: &[u8],
        _absolute_position: Size,
    ) -> Poll<Result<usize>> {
        Poll::Ready(Err(Error::UnsupportedOperation))
    }

    fn flush(&self, _cx: &mut core::task::Context<'_>) -> Poll<Result<()>> {
        Poll::Ready(Ok(()))
    }

    fn control(
        &self,
        _cx: &mut core::task::Context<'_>,
        command: ControlCommandIdentifier,
        _input: &AnyByLayout,
        output: &mut AnyByLayout,
    ) -> Poll<Result<()>> {
        match command {
            IS_A_TERMINAL::IDENTIFIER => {
                let output: &mut bool = IS_A_TERMINAL::cast_output(output)?;
                *output = true;
                Poll::Ready(Ok(()))
            }
            _ => Poll::Ready(Err(Error::UnsupportedOperation)),
        }
    }

    fn cancel_operation(&self, operation: Operation) {
        if operation == Operation::Read {
            critical_section::with(|cs| {
                let mut input = INPUT_QUEUE.borrow(cs).borrow_mut();
                // A spurious wake is harmless; dropping the registration could
                // strand another reader.
                input.reader_waker.wake();
            });
        }
    }
}

impl MountOperations for StandardInDevice {}
impl DirectCharacterDevice for StandardInDevice {}

/// VFS character device representing standard output and standard error over USB serial.
#[derive(Clone, Copy, Default)]
pub struct StandardOutDevice;

impl DirectBaseOperations for StandardOutDevice {
    fn read(
        &self,
        _cx: &mut core::task::Context<'_>,
        _buffer: &mut [u8],
        _absolute_position: Size,
    ) -> Poll<Result<usize>> {
        Poll::Ready(Err(Error::UnsupportedOperation))
    }

    fn write(
        &self,
        cx: &mut core::task::Context<'_>,
        buffer: &[u8],
        _absolute_position: Size,
    ) -> Poll<Result<usize>> {
        if buffer.is_empty() {
            return Poll::Ready(Ok(0));
        }

        critical_section::with(|cs| {
            let mut output = OUTPUT_QUEUE.borrow(cs).borrow_mut();
            let host_connected = DTR_CONNECTED.load(Ordering::SeqCst);

            if host_connected {
                if output.queue.is_full() {
                    output.writer_waker.register(cx.waker());
                    Poll::Pending
                } else {
                    let n = output.queue.push_slice(buffer);
                    output.runner_waker.wake();
                    Poll::Ready(Ok(n))
                }
            } else {
                let n = output.queue.push_slice_overwrite(buffer);
                output.runner_waker.wake();
                Poll::Ready(Ok(n))
            }
        })
    }

    fn flush(&self, cx: &mut core::task::Context<'_>) -> Poll<Result<()>> {
        critical_section::with(|cs| {
            let mut output = OUTPUT_QUEUE.borrow(cs).borrow_mut();
            let host_connected = DTR_CONNECTED.load(Ordering::SeqCst);

            if !host_connected || output.queue.is_empty() {
                Poll::Ready(Ok(()))
            } else {
                output.writer_waker.register(cx.waker());
                Poll::Pending
            }
        })
    }

    fn control(
        &self,
        _cx: &mut core::task::Context<'_>,
        command: ControlCommandIdentifier,
        _input: &AnyByLayout,
        output: &mut AnyByLayout,
    ) -> Poll<Result<()>> {
        match command {
            IS_A_TERMINAL::IDENTIFIER => {
                let output: &mut bool = IS_A_TERMINAL::cast_output(output)?;
                *output = true;
                Poll::Ready(Ok(()))
            }
            _ => Poll::Ready(Err(Error::UnsupportedOperation)),
        }
    }

    fn cancel_operation(&self, operation: Operation) {
        if operation == Operation::Write || operation == Operation::Flush {
            critical_section::with(|cs| {
                let mut output = OUTPUT_QUEUE.borrow(cs).borrow_mut();
                // Standard output and error share this registration: wake rather
                // than drop what may be the other writer's.
                output.writer_waker.wake();
            });
        }
    }
}

impl MountOperations for StandardOutDevice {}
impl DirectCharacterDevice for StandardOutDevice {}

/// Logger type implementing `log::LoggerTrait` formatting into the device-to-host queue.
#[derive(Clone, Copy, Default)]
pub struct Logger;

pub static LOGGER: Logger = Logger;

impl log::LoggerTrait for Logger {
    fn enabled(&self, level: log::Level) -> bool {
        match level {
            log::Level::Error => true,
            log::Level::Warn => true,
            log::Level::Info => true,
            log::Level::Debug => false,
            log::Level::Trace => false,
        }
    }

    fn write(&self, arguments: core::fmt::Arguments) {
        critical_section::with(|cs| {
            let mut output = OUTPUT_QUEUE.borrow(cs).borrow_mut();
            let mut writer = QueueWriter(&mut output.queue);
            let _ = core::fmt::write(&mut writer, arguments);
            output.queue.push_slice_overwrite(b"\r\n");
            output.runner_waker.wake();
        });
    }

    fn flush(&self) {}
}

/// Runner task owning the USB device and CDC-ACM endpoints.
pub struct UsbConsoleRunner {
    usb: UsbDevice<'static, Driver<'static, HardwareVbusDetect>>,
    sender: Sender<'static, Driver<'static, HardwareVbusDetect>>,
    receiver: Receiver<'static, Driver<'static, HardwareVbusDetect>>,
    control: ControlChanged<'static>,
    on_1200_bps_touch: Option<fn()>,
}

impl UsbConsoleRunner {
    /// Run the USB device stack and pump bytes between endpoints and queues.
    pub async fn run(self) -> ! {
        let UsbConsoleRunner {
            mut usb,
            mut sender,
            mut receiver,
            control,
            on_1200_bps_touch,
        } = self;

        let usb_fut = usb.run();

        let rx_fut = async {
            let mut packet = [0u8; 64];
            loop {
                match receiver.read_packet(&mut packet).await {
                    Ok(n) if n > 0 => {
                        let mut offset = 0;
                        while offset < n {
                            let written = critical_section::with(|cs| {
                                let mut input = INPUT_QUEUE.borrow(cs).borrow_mut();
                                let pushed = input.queue.push_slice(&packet[offset..n]);
                                if pushed > 0 {
                                    input.reader_waker.wake();
                                }
                                pushed
                            });
                            offset += written;
                            if offset < n {
                                poll_fn(|cx| {
                                    critical_section::with(|cs| {
                                        let mut input = INPUT_QUEUE.borrow(cs).borrow_mut();
                                        if input.queue.available_space() > 0 {
                                            Poll::Ready(())
                                        } else {
                                            input.runner_waker.register(cx.waker());
                                            Poll::Pending
                                        }
                                    })
                                })
                                .await;
                            }
                        }
                    }
                    Ok(_) => {}
                    Err(_) => {
                        receiver.wait_connection().await;
                    }
                }
            }
        };

        // Transmission and line state share one task: only the sender can read
        // the line coding, and a packet the host never reads must not hide the
        // port being closed.
        let tx_fut = async {
            let mut packet = [0u8; 64];
            loop {
                let dtr = control.dtr();
                if dtr {
                    HOST_SEEN.store(true, Ordering::Relaxed);
                }
                if DTR_CONNECTED.swap(dtr, Ordering::SeqCst) && !dtr {
                    // Writers waiting on back-pressure fall back to dropping.
                    critical_section::with(|cs| {
                        OUTPUT_QUEUE.borrow(cs).borrow_mut().writer_waker.wake();
                    });
                }
                if is_1200_bps_touch(sender.line_coding().data_rate(), dtr)
                    && let Some(hook) = on_1200_bps_touch
                {
                    hook();
                }
                if !dtr {
                    control.control_changed().await;
                    continue;
                }

                let (count, ends_on_full_packet) = critical_section::with(|cs| {
                    let mut output = OUTPUT_QUEUE.borrow(cs).borrow_mut();
                    let popped = output.queue.pop_slice(&mut packet);
                    if popped > 0 {
                        output.writer_waker.wake();
                    }
                    (popped, popped == packet.len() && output.queue.is_empty())
                });

                if count == 0 {
                    let output_ready = poll_fn(|cx| {
                        critical_section::with(|cs| {
                            let mut output = OUTPUT_QUEUE.borrow(cs).borrow_mut();
                            if output.queue.is_empty() {
                                output.runner_waker.register(cx.waker());
                                Poll::Pending
                            } else {
                                Poll::Ready(())
                            }
                        })
                    });
                    select(output_ready, control.control_changed()).await;
                    continue;
                }

                if send(&mut sender, &control, &packet[..count]).await && ends_on_full_packet {
                    // A transfer that ends on a full packet needs an empty one
                    // for the host to deliver it.
                    send(&mut sender, &control, &[]).await;
                }
            }
        };

        join3(usb_fut, rx_fut, tx_fut).await;
        loop {
            core::future::pending::<()>().await;
        }
    }
}

/// Sends one packet, giving it up if the host closes the port meanwhile.
/// Returns whether the host took it.
async fn send(
    sender: &mut Sender<'static, Driver<'static, HardwareVbusDetect>>,
    control: &ControlChanged<'static>,
    packet: &[u8],
) -> bool {
    let mut write = pin!(sender.write_packet(packet));
    loop {
        match select(write.as_mut(), control.control_changed()).await {
            Either::First(Ok(())) => return true,
            // The bus is not configured: the next line change says when it is.
            Either::First(Err(_)) => {
                control.control_changed().await;
                return false;
            }
            Either::Second(()) if !control.dtr() => return false,
            Either::Second(()) => {}
        }
    }
}

/// Structure containing standard I/O devices and the background USB runner.
pub struct UsbConsole {
    pub stdin: StandardInDevice,
    pub stdout: StandardOutDevice,
    pub runner: UsbConsoleRunner,
}

impl UsbConsole {
    /// Initialize the USB serial console (CDC-ACM) subsystem.
    ///
    /// # Board Crate Clock Configuration Requirement
    ///
    /// The nRF52840 USB peripheral requires the external 32 MHz high-frequency crystal oscillator (HFXO)
    /// to be running for accurate USB clocking. By default, `embassy_nrf::config::Config::default()` selects
    /// `HfclkSource::Internal` (the internal 64 MHz RC oscillator), which does not satisfy USB timing requirements.
    ///
    /// The board crate **must** explicitly configure the external crystal before calling `embassy_nrf::init`:
    ///
    /// ```rust,no_run
    /// let mut config = embassy_nrf::config::Config::default();
    /// config.hfclk_source = embassy_nrf::config::HfclkSource::ExternalXtal;
    /// let peripherals = embassy_nrf::init(config);
    /// ```
    ///
    /// Failure to set `hfclk_source = HfclkSource::ExternalXtal` will result in the USB peripheral
    /// failing to enumerate or clock packets properly on the host.
    pub fn new(usbd: Peri<'static, USBD>, config: UsbConsoleConfig) -> Result<Self> {
        if INITIALIZED.swap(true, Ordering::SeqCst) {
            return Err(Error::RessourceBusy);
        }

        let vbus = HardwareVbusDetect::new(Irqs);
        let driver = Driver::new(usbd, Irqs, vbus);

        let mut usb_config = embassy_usb::Config::new(config.vendor_id, config.product_id);
        usb_config.manufacturer = config.manufacturer;
        usb_config.product = config.product;
        usb_config.serial_number = config.serial_number;
        usb_config.max_power = 100;
        usb_config.max_packet_size_0 = 64;

        // SAFETY: INITIALIZED ensures this block executes at most once in the system's lifetime.
        // No concurrent execution or repeated call can obtain another mutable reference to USB_BUFFERS.
        let buffers = unsafe { &mut *core::ptr::addr_of_mut!(USB_BUFFERS) };

        let mut builder = Builder::new(
            driver,
            usb_config,
            &mut buffers.config_descriptor,
            &mut buffers.bos_descriptor,
            &mut buffers.msos_descriptor,
            &mut buffers.control,
        );

        let class = CdcAcmClass::new(&mut builder, &mut buffers.cdc_state, 64);
        let usb = builder.build();
        let (sender, receiver, control) = class.split_with_control();

        Ok(Self {
            stdin: StandardInDevice,
            stdout: StandardOutDevice,
            runner: UsbConsoleRunner {
                usb,
                sender,
                receiver,
                control,
                on_1200_bps_touch: config.on_1200_bps_touch,
            },
        })
    }
}
