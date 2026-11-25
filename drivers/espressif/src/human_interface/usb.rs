//! The composite USB device of the ESP32-S3: CDC-ACM (the console) and HID (the
//! human interface emulation) on the USB OTG controller, over `embassy-usb` on
//! esp-hal's OTG driver.
//!
//! The ESP32-S3 has one USB PHY, shared by the USB Serial/JTAG controller and the
//! USB OTG controller, so a board whose USB port carries the console through the
//! OTG controller runs the console as the CDC-ACM interface of this composite
//! device, and the USB Serial/JTAG controller stays unused.

use core::cell::RefCell;
use core::future::poll_fn;
use core::sync::atomic::{AtomicBool, AtomicU8, Ordering};
use core::task::Poll;

use critical_section::Mutex;
use embassy_futures::join::{join, join3};
use embassy_sync::waitqueue::WakerRegistration;
use embassy_usb::class::cdc_acm::{
    CdcAcmClass, ControlChanged, Receiver, Sender, State as CdcAcmState,
};
use embassy_usb::class::hid::{
    Config as HidConfig, HidBootProtocol, HidReader, HidReaderWriter, HidSubclass, HidWriter,
    ReadError, ReportId, RequestHandler, State as HidState,
};
use embassy_usb::control::OutResponse;
use embassy_usb::{Builder, Handler, UsbDevice};
use esp_hal::peripherals::USB_FS;
use esp_hal::usb::otg::embassy_usb_device::{Config as OtgConfig, Driver as OtgDriver};
use esp_hal::usb::otg::{Usb, UsbFsDm, UsbFsDp};
use file_system::IoErrorKind;
use peripherals::human_interface::{
    Error, HumanInterfaceController, REPORT_DESCRIPTOR, REPORT_IDENTIFIER_CONSUMER,
    REPORT_IDENTIFIER_KEYBOARD, REPORT_IDENTIFIER_MOUSE, REPORT_IDENTIFIER_SYSTEM,
};

use super::logic::{
    BusEvent, CONTROL_BUFFER_SIZE, ConnectionState, ENDPOINT_MAX_PACKET_SIZE,
    ENDPOINT_OUT_BUFFER_SIZE, HID_POLL_MILLISECONDS, REPORT_QUEUE_CAPACITY, ReportQueue,
    frame_report, led_indicators_from_control_payload, led_indicators_from_output_report,
    release_report, report_length,
};
use crate::console::ConsolePort;

/// The USB OTG full-speed driver type this module builds the device on.
type Otg = OtgDriver<'static>;

/// The four report identifiers of [`REPORT_DESCRIPTOR`], to send the release
/// reports when the host comes back.
const REPORT_IDENTIFIERS: [u8; 4] = [
    REPORT_IDENTIFIER_KEYBOARD,
    REPORT_IDENTIFIER_CONSUMER,
    REPORT_IDENTIFIER_MOUSE,
    REPORT_IDENTIFIER_SYSTEM,
];

/// USB identifiers and strings of the composite device.
#[derive(Debug, Clone, Copy)]
pub struct UsbHumanInterfaceConfig {
    pub vendor_id: u16,
    pub product_id: u16,
    pub manufacturer: Option<&'static str>,
    pub product: Option<&'static str>,
    pub serial_number: Option<&'static str>,
}

impl Default for UsbHumanInterfaceConfig {
    fn default() -> Self {
        Self {
            // The test identifiers of pid.codes, as the Nordic USB console uses.
            vendor_id: 0x1209,
            product_id: 0x0001,
            manufacturer: Some("Xila"),
            product: Some("Xila USB console and human interface"),
            serial_number: None,
        }
    }
}

struct ReportState {
    queue: ReportQueue<REPORT_QUEUE_CAPACITY>,
    runner_waker: WakerRegistration,
}

static CONNECTION: Mutex<RefCell<ConnectionState>> =
    Mutex::new(RefCell::new(ConnectionState::new()));
static REPORT_QUEUE: Mutex<RefCell<ReportState>> = Mutex::new(RefCell::new(ReportState {
    queue: ReportQueue::new(),
    runner_waker: WakerRegistration::new(),
}));
/// Keyboard LED bitmask last reported by the host.
static INDICATORS: AtomicU8 = AtomicU8::new(0);
static INITIALIZED: AtomicBool = AtomicBool::new(false);

/// Runs `function` with the connection state and the report queue, under one
/// critical section. Every access takes the two in this order.
fn with_usb_state<T>(function: impl FnOnce(&mut ConnectionState, &mut ReportState) -> T) -> T {
    critical_section::with(|section| {
        let mut connection = CONNECTION.borrow(section).borrow_mut();
        let mut state = REPORT_QUEUE.borrow(section).borrow_mut();
        function(&mut connection, &mut state)
    })
}

/// Applies a USB bus event to the connection state and the report queue: on an
/// event that disconnects, the queued reports are dropped and the LED state is
/// cleared; when the host comes back, one all-zero release report per identifier
/// is queued, so that no key stays held.
fn apply_bus_event(event: BusEvent) {
    with_usb_state(|connection, state| {
        let outcome = connection.apply(event);
        if outcome.invalidate {
            state.queue.clear();
            INDICATORS.store(0, Ordering::Relaxed);
        }
        if outcome.host_returned {
            for identifier in REPORT_IDENTIFIERS {
                if let Some((bytes, length)) = release_report(identifier) {
                    // Best effort: a full queue keeps the older reports.
                    let _ = state.queue.push(identifier, &bytes[..length]);
                }
            }
        }
        state.runner_waker.wake();
    });
}

/// Answers the HID control requests that carry an output report: SET_REPORT with
/// the keyboard report identifier updates the LED bitmask, and GET_REPORT reads
/// the LED state back or answers an all-zero input report. The boot protocol
/// stays rejected, as [`REPORT_DESCRIPTOR`] does not declare it.
struct HidRequestHandler;

impl RequestHandler for HidRequestHandler {
    fn get_report(&mut self, identifier: ReportId, buffer: &mut [u8]) -> Option<usize> {
        match identifier {
            ReportId::Out(REPORT_IDENTIFIER_KEYBOARD) => {
                if buffer.is_empty() {
                    return None;
                }
                buffer[0] = INDICATORS.load(Ordering::Relaxed);
                Some(1)
            }
            ReportId::In(identifier) => {
                let length = report_length(identifier)?;
                if buffer.len() < length {
                    return None;
                }
                buffer[..length].fill(0);
                Some(length)
            }
            _ => None,
        }
    }

    fn set_report(&mut self, identifier: ReportId, payload: &[u8]) -> OutResponse {
        match identifier {
            ReportId::Out(REPORT_IDENTIFIER_KEYBOARD) => {
                if let Some(indicators) =
                    led_indicators_from_control_payload(REPORT_IDENTIFIER_KEYBOARD, payload)
                {
                    INDICATORS.store(indicators, Ordering::Relaxed);
                }
                OutResponse::Accepted
            }
            _ => OutResponse::Rejected,
        }
    }
}

/// Follows the USB bus events: bus reset, configuration, suspend and power. esp-hal
/// forces VBUSVALID, so an unplugged cable shows up as a suspend, not as a power
/// removal, and embassy-usb does not disable the endpoints on suspend, so the
/// connection state has to be tracked here.
struct BusEventHandler;

impl Handler for BusEventHandler {
    fn enabled(&mut self, enabled: bool) {
        if !enabled {
            apply_bus_event(BusEvent::PowerRemoved);
        }
    }

    fn reset(&mut self) {
        apply_bus_event(BusEvent::Reset);
    }

    fn configured(&mut self, configured: bool) {
        apply_bus_event(if configured {
            BusEvent::Configured
        } else {
            BusEvent::Unconfigured
        });
    }

    fn suspended(&mut self, suspended: bool) {
        apply_bus_event(if suspended {
            BusEvent::Suspended
        } else {
            BusEvent::Resumed
        });
    }
}

struct UsbBuffers {
    config_descriptor: [u8; 256],
    bos_descriptor: [u8; 16],
    msos_descriptor: [u8; 16],
    control: [u8; CONTROL_BUFFER_SIZE],
    endpoint_out: [u8; ENDPOINT_OUT_BUFFER_SIZE],
    cdc_state: CdcAcmState<'static>,
    hid_state: HidState<'static>,
    hid_request_handler: HidRequestHandler,
    bus_event_handler: BusEventHandler,
}

static mut USB_BUFFERS: UsbBuffers = UsbBuffers {
    config_descriptor: [0; 256],
    bos_descriptor: [0; 16],
    msos_descriptor: [0; 16],
    control: [0; CONTROL_BUFFER_SIZE],
    endpoint_out: [0; ENDPOINT_OUT_BUFFER_SIZE],
    cdc_state: CdcAcmState::new(),
    hid_state: HidState::new(),
    hid_request_handler: HidRequestHandler,
    bus_event_handler: BusEventHandler,
};

/// The runner of the composite device, stored by [`UsbHumanInterface::new`] and
/// taken once by the console task through [`take_runner`]. The embassy-usb types it
/// owns are not `Send`, so the runner does not travel inside `ConsolePort` (which
/// the firmware keeps in a static): it stays here until the console task takes it.
static mut USB_RUNNER: Option<UsbCompositeRunner> = None;

/// Takes the runner that [`UsbHumanInterface::new`] stored. `None` when it was
/// already taken.
pub(crate) fn take_runner() -> Option<UsbCompositeRunner> {
    // SAFETY: `UsbHumanInterface::new` writes the slot once (guarded by
    // INITIALIZED). `take` moves the runner out and leaves `None` in the slot, so a
    // second call returns `None` instead of copying the value, and no reference to
    // the slot is created.
    unsafe { (*core::ptr::addr_of_mut!(USB_RUNNER)).take() }
}

/// The human interface emulation controller of the composite USB device.
///
/// It only touches the shared connection state and report queue: the USB runner
/// task owns the endpoints. `send_report` queues the report and wakes the runner;
/// it fails with [`Error::Busy`] when the queue is full, so no report is dropped
/// silently, and with [`Error::NotConnected`] while no computer is connected and
/// listening.
pub struct UsbHumanInterfaceController;

impl UsbHumanInterfaceController {
    const fn new() -> Self {
        Self
    }
}

impl HumanInterfaceController for UsbHumanInterfaceController {
    fn send_report(&mut self, identifier: u8, report: &[u8]) -> Result<(), Error> {
        with_usb_state(|connection, state| {
            if !connection.is_connected() {
                return Err(Error::NotConnected);
            }
            // Busy when the queue is full: the report is not dropped silently.
            state.queue.push(identifier, report)?;
            state.runner_waker.wake();
            Ok(())
        })
    }

    fn is_connected(&self) -> bool {
        with_usb_state(|connection, _| connection.is_connected())
    }

    fn indicators(&self) -> u8 {
        INDICATORS.load(Ordering::Relaxed)
    }
}

/// The composite USB device: the CDC-ACM console and the HID human interface
/// emulation on the USB OTG controller.
pub struct UsbHumanInterface {
    /// The console port: the CDC-ACM interface of the composite device, handed to
    /// the firmware's `Board`.
    pub console: ConsolePort,
    /// The human interface controller, mounted at
    /// `/devices/human_interface/emulation` through the upper half.
    pub controller: UsbHumanInterfaceController,
}

impl UsbHumanInterface {
    /// Builds the composite device on the USB OTG full-speed controller.
    ///
    /// `data_plus` and `data_minus` are the USB D+ and D- pins (GPIO 20 and GPIO 19
    /// on the ESP32-S3). Call once: the USB PHY is a singleton.
    pub fn new(
        peripheral: USB_FS<'static>,
        data_plus: impl UsbFsDp + 'static,
        data_minus: impl UsbFsDm + 'static,
        config: UsbHumanInterfaceConfig,
    ) -> Result<Self, IoErrorKind> {
        if INITIALIZED.swap(true, Ordering::SeqCst) {
            return Err(IoErrorKind::ResourceBusy);
        }

        let usb = Usb::new_fs(peripheral, data_plus, data_minus);
        // SAFETY: INITIALIZED ensures this block runs at most once in the system's
        // lifetime, so no other reference to USB_BUFFERS exists.
        let buffers = unsafe { &mut *core::ptr::addr_of_mut!(USB_BUFFERS) };
        let driver = OtgDriver::new(usb, &mut buffers.endpoint_out, OtgConfig::default());

        let mut usb_config = embassy_usb::Config::new(config.vendor_id, config.product_id);
        usb_config.manufacturer = config.manufacturer;
        usb_config.product = config.product;
        usb_config.serial_number = config.serial_number;
        usb_config.max_power = 100;
        usb_config.max_packet_size_0 = ENDPOINT_MAX_PACKET_SIZE as u8;

        let mut builder = Builder::new(
            driver,
            usb_config,
            &mut buffers.config_descriptor,
            &mut buffers.bos_descriptor,
            &mut buffers.msos_descriptor,
            &mut buffers.control,
        );

        let cdc_class = CdcAcmClass::new(
            &mut builder,
            &mut buffers.cdc_state,
            ENDPOINT_MAX_PACKET_SIZE as u16,
        );
        let hid = HidReaderWriter::<Otg, 64, 64>::new(
            &mut builder,
            &mut buffers.hid_state,
            HidConfig {
                report_descriptor: REPORT_DESCRIPTOR,
                request_handler: Some(&mut buffers.hid_request_handler),
                poll_ms: HID_POLL_MILLISECONDS,
                max_packet_size: ENDPOINT_MAX_PACKET_SIZE as u16,
                hid_subclass: HidSubclass::No,
                hid_boot_protocol: HidBootProtocol::None,
            },
        );
        builder.handler(&mut buffers.bus_event_handler);
        let usb_device = builder.build();

        let (cdc_sender, cdc_receiver, cdc_control) = cdc_class.split_with_control();
        let (hid_reader, hid_writer) = hid.split();

        // SAFETY: INITIALIZED ensures this runs at most once, so the slot is
        // written exactly once, before the console task takes the runner.
        unsafe {
            core::ptr::addr_of_mut!(USB_RUNNER).write(Some(UsbCompositeRunner {
                usb: usb_device,
                cdc_sender,
                cdc_receiver,
                cdc_control,
                hid_reader,
                hid_writer,
            }));
        }

        Ok(Self {
            console: ConsolePort::UsbComposite,
            controller: UsbHumanInterfaceController::new(),
        })
    }
}

/// Runs the composite device: the USB stack, the CDC-ACM console pump and the HID
/// report pump. The firmware's console task runs it.
pub struct UsbCompositeRunner {
    usb: UsbDevice<'static, Otg>,
    cdc_sender: Sender<'static, Otg>,
    cdc_receiver: Receiver<'static, Otg>,
    cdc_control: ControlChanged<'static>,
    hid_reader: HidReader<'static, Otg, 64>,
    hid_writer: HidWriter<'static, Otg, 64>,
}

impl UsbCompositeRunner {
    /// Runs the USB device until the firmware stops.
    pub async fn run(self) -> ! {
        let Self {
            mut usb,
            cdc_sender,
            cdc_receiver,
            cdc_control,
            hid_reader,
            hid_writer,
        } = self;
        let usb_future = usb.run();
        let console_future = pump_console(cdc_sender, cdc_receiver, cdc_control);
        let hid_future = pump_hid(hid_reader, hid_writer);
        join3(usb_future, console_future, hid_future).await;
        loop {
            core::future::pending::<()>().await;
        }
    }
}

/// Pumps the CDC-ACM endpoints into the console's input and output queues, like the
/// USB Serial/JTAG transport of [`crate::console`] does.
async fn pump_console(
    mut sender: Sender<'static, Otg>,
    mut receiver: Receiver<'static, Otg>,
    control: ControlChanged<'static>,
) {
    let receive_future = async {
        let mut packet = [0u8; ENDPOINT_MAX_PACKET_SIZE];
        let mut last_warning = None;
        loop {
            match receiver.read_packet(&mut packet).await {
                Ok(count) if count > 0 => {
                    if crate::console::accept_input(&packet[..count]) {
                        crate::console::report_overflow(&mut last_warning);
                    }
                }
                Ok(_) => {}
                Err(_) => receiver.wait_connection().await,
            }
        }
    };

    let transmit_future = async {
        let mut packet = [0u8; ENDPOINT_MAX_PACKET_SIZE];
        loop {
            if !control.dtr() {
                // The host has no terminal open: keep the queued output until one
                // does, as the USB Serial/JTAG transport waits for the port too.
                control.control_changed().await;
                continue;
            }
            let count = crate::console::next_output(&mut packet).await;
            let ends_on_full_packet = count == packet.len() && crate::console::output_is_empty();
            match sender.write_packet(&packet[..count]).await {
                Ok(()) => {
                    if ends_on_full_packet {
                        // A transfer that ends on a full packet needs an empty one
                        // for the host to deliver it.
                        let _ = sender.write_packet(&[]).await;
                    }
                    if crate::console::output_is_empty() {
                        crate::console::finish_transmission();
                    }
                }
                // The bus is not configured: the packet is lost, as a failed write
                // cannot be queued again.
                Err(_) => {
                    crate::console::finish_transmission();
                    control.control_changed().await;
                }
            }
        }
    };

    join(receive_future, transmit_future).await;
}

/// Sends the queued HID reports on the interrupt IN endpoint and reads the LED
/// output report on the interrupt OUT endpoint.
async fn pump_hid(
    mut reader: HidReader<'static, Otg, 64>,
    mut writer: HidWriter<'static, Otg, 64>,
) {
    let write_future = async {
        loop {
            // Pop one report and capture the connection generation with it, so that
            // a report popped before a bus event is not delivered after it.
            let (generation, identifier, bytes, length) = poll_fn(|context| {
                with_usb_state(|connection, state| match state.queue.pop() {
                    Some((identifier, bytes, length)) => {
                        Poll::Ready((connection.generation(), identifier, bytes, length))
                    }
                    None => {
                        state.runner_waker.register(context.waker());
                        Poll::Pending
                    }
                })
            })
            .await;
            // A bus event happened between the pop and now: drop the report.
            let still_current =
                with_usb_state(|connection, _| connection.generation() == generation);
            if !still_current {
                continue;
            }
            // The descriptor declares report identifiers, so the frame on the wire
            // starts with the identifier byte.
            let Some((frame, frame_length)) = frame_report(identifier, &bytes[..length]) else {
                continue;
            };
            if writer.write(&frame[..frame_length]).await.is_err() {
                // Second defence: the endpoint was disabled (bus reset or
                // unconfiguration). The report is dropped, not retried; wait for
                // the endpoint to be enabled again by a new configuration.
                writer.ready().await;
            }
        }
    };

    let read_future = async {
        let mut report = [0u8; ENDPOINT_MAX_PACKET_SIZE];
        loop {
            match reader.read(&mut report).await {
                Ok(count) => {
                    if let Some(indicators) = led_indicators_from_output_report(&report[..count]) {
                        INDICATORS.store(indicators, Ordering::Relaxed);
                    }
                }
                Err(ReadError::Disabled) => reader.ready().await,
                Err(_) => {}
            }
        }
    };

    join(write_future, read_future).await;
}
