#[cfg(target_feature = "atomics")]
compile_error!("the browser audio bridge requires single-threaded browser WASM");

use alloc::{
    boxed::Box,
    format,
    string::{String, ToString},
};
use core::{
    array, fmt,
    future::{Future, poll_fn},
    pin::{Pin, pin},
    sync::atomic::{AtomicBool, AtomicU32, AtomicU64, AtomicUsize, Ordering},
    task::Poll,
};

use audio::{
    AudioSystem, Configuration, GET_CONFIGURATION, GET_VOLUME, GET_WRITABLE, OutputHandle,
    SET_CONFIGURATION, SET_VOLUME, START, STOP, SampleFormat, StreamHandle,
};
use file_system::{
    BaseOperations, CharacterDevice, Context, ControlCommand, ControlCommandIdentifier,
    MountOperations, Size,
};
use io_error::{Kind, Result};
use js_sys::{Array, ArrayBuffer, Object, Promise, Reflect, Uint8Array};
use nanoaudio_web::{Format, Options, Output, Port, Source, Worklet};
use shared::AnyByLayout;
use wasm_bindgen::{JsCast, JsValue, prelude::wasm_bindgen};
use wasm_bindgen_futures::JsFuture;

const STREAMS: usize = 4;
// Bridge credit and worklet PCM storage per stream: about 21 ms at 48 kHz stereo F32.
const STREAM_BYTES: usize = 8192;
const MIX_FRAMES: usize = 128;
const OUTPUTS: usize = 1;
const SINK_BYTES: usize = MIX_FRAMES * 2 * size_of::<f32>() * 3;
const FRAME_BYTES: usize = 2 * size_of::<f32>();
const MIX_BLOCK_BYTES: usize = MIX_FRAMES * FRAME_BYTES;
const MAX_TRANSFER_BYTES: usize = MIX_BLOCK_BYTES;
const MAX_CONTROL_BYTES: usize = 256;
const MAX_CONTROL_MESSAGES: usize = 16;
const OUTPUT_CONFIGURATION: audio::Configuration = Configuration::new(48_000, 2, SampleFormat::F32);

/// How long loading the audio worklet may take: it fetches the application's JavaScript glue again.
const OPEN_TIMEOUT_MS: i32 = 30_000;
/// How long the browser may keep the audio context suspended after the worklet is loaded.
///
/// Browsers leave `AudioContext.resume()` pending, without rejecting it, while autoplay is blocked.
const RESUME_TIMEOUT_MS: i32 = 10_000;
/// The longest browser error text shown to the user; the console log keeps the whole text.
const MAX_SUMMARY_CHARACTERS: usize = 160;

static STARTED: AtomicBool = AtomicBool::new(false);

/// Why [`WebAudioDevice::start_from_user_gesture`] failed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StartError {
    /// Another start is still running, or the browser audio device already exists.
    Busy,
    /// Creating the audio context or loading the audio worklet failed, with the browser's error.
    Open(String),
    /// Loading the audio worklet did not finish in time.
    OpenTimedOut,
    /// Starting the audio context failed, with the browser's error.
    Resume(String),
    /// The browser kept the audio context suspended, which happens while it blocks autoplay for the site.
    ResumeTimedOut,
}

impl StartError {
    /// Returns a short sentence for the page, with the browser's error when there is one.
    pub fn summary(&self) -> String {
        match self {
            Self::Busy => "Browser audio is already starting or running.".to_string(),
            Self::Open(error) => format!(
                "The browser could not load the audio worklet: {}",
                shorten(error)
            ),
            Self::OpenTimedOut => format!(
                "Loading the audio worklet took more than {} seconds.",
                OPEN_TIMEOUT_MS / 1000
            ),
            Self::Resume(error) => {
                format!("The browser refused to start audio: {}", shorten(error))
            }
            Self::ResumeTimedOut => format!(
                "The browser did not start audio within {} seconds. It is probably blocking audio for this site: allow audio and video for this site in the browser's site permissions (Firefox: the autoplay icon or the padlock in the address bar), then retry.",
                RESUME_TIMEOUT_MS / 1000
            ),
        }
    }
}

impl fmt::Display for StartError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Busy => formatter.write_str("browser audio is already starting or running"),
            Self::Open(error) => write!(
                formatter,
                "creating the audio context or loading the worklet failed: {error}"
            ),
            Self::OpenTimedOut => write!(
                formatter,
                "loading the audio worklet did not finish within {OPEN_TIMEOUT_MS} ms"
            ),
            Self::Resume(error) => write!(formatter, "starting the audio context failed: {error}"),
            Self::ResumeTimedOut => write!(
                formatter,
                "the audio context was still suspended {RESUME_TIMEOUT_MS} ms after the worklet loaded (autoplay is probably blocked for this site)"
            ),
        }
    }
}

fn shorten(text: &str) -> String {
    match text.char_indices().nth(MAX_SUMMARY_CHARACTERS) {
        Some((end, _)) => format!("{}…", &text[..end]),
        None => text.to_string(),
    }
}

/// Describes a JavaScript exception as `Name: message` instead of the opaque `JsValue(...)`.
fn describe_value(value: &JsValue) -> String {
    let text = |key: &str| {
        Reflect::get(value, &JsValue::from_str(key))
            .ok()
            .and_then(|field| field.as_string())
            .filter(|field| !field.is_empty())
    };
    match (text("name"), text("message")) {
        (Some(name), Some(message)) => format!("{name}: {message}"),
        (None, Some(message)) => message,
        (Some(name), None) => name,
        (None, None) => value.as_string().unwrap_or_else(|| format!("{value:?}")),
    }
}

fn describe(error: &nanoaudio_web::Error) -> String {
    match error {
        nanoaudio_web::Error::Backend(value) => describe_value(value),
        error => error.to_string(),
    }
}

/// Reports `navigator.userActivation`: whether the click still counts as a gesture, and whether the page was ever activated.
fn activation() -> String {
    let flag = |key: &str| {
        let window = web_sys::window()?;
        let activation =
            Reflect::get(window.navigator().as_ref(), &"userActivation".into()).ok()?;
        Reflect::get(&activation, &key.into()).ok()?.as_bool()
    };
    format!(
        "user activation active={:?} sticky={:?}",
        flag("isActive"),
        flag("hasBeenActive")
    )
}

/// A `setTimeout` that is cleared when dropped.
struct Deadline {
    handle: Option<i32>,
    expired: JsFuture,
}

impl Deadline {
    fn after(milliseconds: i32) -> Self {
        let window = web_sys::window();
        let mut handle = None;
        let promise = Promise::new(&mut |resolve, _| {
            handle = window.as_ref().and_then(|window| {
                window
                    .set_timeout_with_callback_and_timeout_and_arguments_0(&resolve, milliseconds)
                    .ok()
            });
        });
        Self {
            handle,
            expired: JsFuture::from(promise),
        }
    }
}

impl Drop for Deadline {
    fn drop(&mut self) {
        if let (Some(handle), Some(window)) = (self.handle, web_sys::window()) {
            window.clear_timeout_with_handle(handle);
        }
    }
}

/// Polls `future` until it completes or `milliseconds` elapse, and drops it in the second case.
async fn within<F: Future>(milliseconds: i32, future: F) -> Option<F::Output> {
    let mut future = pin!(future);
    let mut deadline = Deadline::after(milliseconds);
    poll_fn(|context| {
        if let Poll::Ready(output) = future.as_mut().poll(context) {
            return Poll::Ready(Some(output));
        }
        match Pin::new(&mut deadline.expired).poll(context) {
            Poll::Ready(_) => Poll::Ready(None),
            Poll::Pending => Poll::Pending,
        }
    })
    .await
}

struct StartGuard<'a> {
    started: &'a AtomicBool,
    committed: bool,
}

impl<'a> StartGuard<'a> {
    fn acquire(started: &'a AtomicBool) -> Result<Self> {
        started
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .map_err(|_| Kind::ResourceBusy)?;
        Ok(Self {
            started,
            committed: false,
        })
    }

    fn commit(mut self) {
        self.committed = true;
    }
}

impl Drop for StartGuard<'_> {
    fn drop(&mut self) {
        if !self.committed {
            self.started.store(false, Ordering::Release);
        }
    }
}

struct BridgeSlot {
    occupied: AtomicBool,
    id: AtomicU32,
    sent: AtomicU64,
    consumed: AtomicU64,
    ready: AtomicBool,
    closing: AtomicBool,
}

impl BridgeSlot {
    const fn new() -> Self {
        Self {
            occupied: AtomicBool::new(false),
            id: AtomicU32::new(0),
            sent: AtomicU64::new(0),
            consumed: AtomicU64::new(0),
            ready: AtomicBool::new(false),
            closing: AtomicBool::new(false),
        }
    }

    fn writable(&self) -> usize {
        if !self.ready.load(Ordering::Acquire) {
            return 0;
        }
        let outstanding = self
            .sent
            .load(Ordering::Acquire)
            .saturating_sub(self.consumed.load(Ordering::Acquire));
        STREAM_BYTES.saturating_sub(outstanding.min(STREAM_BYTES as u64) as usize)
    }

    fn reserve(&self, requested: usize) -> usize {
        if !self.ready.load(Ordering::Acquire) {
            return 0;
        }
        loop {
            let sent = self.sent.load(Ordering::Acquire);
            let consumed = self.consumed.load(Ordering::Acquire);
            let outstanding = sent.saturating_sub(consumed);
            let available =
                STREAM_BYTES.saturating_sub(outstanding.min(STREAM_BYTES as u64) as usize);
            let count = requested.min(available);
            if count == 0 {
                return 0;
            }
            if self
                .sent
                .compare_exchange(
                    sent,
                    sent.saturating_add(count as u64),
                    Ordering::AcqRel,
                    Ordering::Acquire,
                )
                .is_ok()
            {
                return count;
            }
        }
    }

    fn undo_reservation(&self, count: usize) {
        self.sent.fetch_sub(count as u64, Ordering::AcqRel);
    }

    fn report_consumed(&self, consumed: u64) {
        self.consumed.fetch_max(
            consumed.min(self.sent.load(Ordering::Acquire)),
            Ordering::AcqRel,
        );
    }
}

struct Bridge {
    slots: [BridgeSlot; STREAMS],
    next_id: AtomicU32,
    pending_controls: AtomicUsize,
    rejected_controls: AtomicU32,
    output: Output,
}

unsafe impl Send for Bridge {}
unsafe impl Sync for Bridge {}

impl Bridge {
    fn slot(&self, id: u32) -> Option<&BridgeSlot> {
        self.slots.iter().find(|slot| {
            slot.occupied.load(Ordering::Acquire) && slot.id.load(Ordering::Acquire) == id
        })
    }

    fn allocate(&self) -> Result<(usize, u32)> {
        let (index, slot) = self
            .slots
            .iter()
            .enumerate()
            .find(|(_, slot)| {
                slot.occupied
                    .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
                    .is_ok()
            })
            .ok_or(Kind::ResourceBusy)?;
        let id = self
            .next_id
            .fetch_add(1, Ordering::Relaxed)
            .wrapping_add(1)
            .max(1);
        slot.sent.store(0, Ordering::Relaxed);
        slot.consumed.store(0, Ordering::Relaxed);
        slot.ready.store(false, Ordering::Relaxed);
        slot.closing.store(false, Ordering::Relaxed);
        slot.id.store(id, Ordering::Release);
        Ok((index, id))
    }

    fn release(&self, slot_index: usize, id: u32) {
        let slot = &self.slots[slot_index];
        if slot.id.load(Ordering::Acquire) == id {
            slot.ready.store(false, Ordering::Release);
            slot.closing.store(false, Ordering::Release);
            slot.occupied.store(false, Ordering::Release);
        }
    }

    fn release_id(&self, id: u32) {
        if let Some((index, _)) = self.slots.iter().enumerate().find(|(_, slot)| {
            slot.occupied.load(Ordering::Acquire) && slot.id.load(Ordering::Acquire) == id
        }) {
            self.release(index, id);
        }
    }

    fn send(&self, packet: &Object, transfer: &[ArrayBuffer]) -> Result<()> {
        let transfers = Array::new();
        for buffer in transfer {
            transfers.push(buffer);
        }
        let value: JsValue = packet.clone().into();
        self.output
            .post_with_transfer(&value, &transfers)
            .map_err(|_| Kind::InputOutput)
    }

    fn send_control(
        &self,
        id: u32,
        command: ControlCommandIdentifier,
        input: &[u8],
        output_size: usize,
    ) -> Result<()> {
        let (bytes, buffer) = make_bytes(input);
        let packet = packet("control", id);
        set(&packet, "command", &JsValue::from_f64(command.raw() as f64))?;
        set(
            &packet,
            "outputSize",
            &JsValue::from_f64(output_size as f64),
        )?;
        set(&packet, "bytes", bytes.as_ref())?;
        self.pending_controls
            .try_update(Ordering::AcqRel, Ordering::Acquire, |count| {
                (count < MAX_CONTROL_MESSAGES).then_some(count + 1)
            })
            .map_err(|_| Kind::WouldBlock)?;
        if let Err(error) = self.send(&packet, &[buffer]) {
            self.pending_controls.fetch_sub(1, Ordering::AcqRel);
            return Err(error);
        }
        Ok(())
    }

    fn on_message(&self, value: JsValue) {
        let Ok(op) = field(&value, "op") else {
            return;
        };
        if op.as_string().as_deref() == Some("control_ack") {
            let _ =
                self.pending_controls
                    .try_update(Ordering::AcqRel, Ordering::Acquire, |count| {
                        Some(count.saturating_sub(1))
                    });
            if let Some(rejection) = control_rejection(&value) {
                // Controls are fire-and-forget: the caller already got `Ok`. A rejection is logged, with growing gaps so that a command repeated in a loop cannot flood the console.
                let count = self
                    .rejected_controls
                    .fetch_add(1, Ordering::Relaxed)
                    .saturating_add(1);
                if count.is_power_of_two() {
                    log::error!(
                        "Browser audio: the worklet rejected {rejection} ({count} rejected control(s) so far)"
                    );
                }
            }
            return;
        }
        let Ok(id) = number_field(&value, "id") else {
            return;
        };
        let Some(slot) = self.slot(id) else {
            return;
        };
        match op.as_string().as_deref() {
            Some("open_ack") => {
                if !slot.closing.load(Ordering::Acquire) {
                    slot.ready.store(true, Ordering::Release);
                }
            }
            Some("close_ack") => self.release_id(id),
            Some("consumed") => {
                if let Ok(consumed) = number_field64(&value, "bytes") {
                    slot.report_consumed(consumed);
                }
            }
            _ => {}
        }
    }
}

pub struct WebAudioDevice {
    bridge: Box<Bridge>,
}

impl WebAudioDevice {
    /// Opens the browser audio output.
    ///
    /// Call it from a click handler and ignore further clicks until it returns: a second concurrent call fails with [`StartError::Busy`]. Each step is logged, and no step stays pending for more than [`OPEN_TIMEOUT_MS`] or [`RESUME_TIMEOUT_MS`].
    pub async fn start_from_user_gesture() -> core::result::Result<Self, StartError> {
        let startup = StartGuard::acquire(&STARTED).map_err(|_| {
            log::warning!(
                "Browser audio: start ignored, another start is running or audio is already running"
            );
            StartError::Busy
        })?;
        log::information!("Browser audio: starting ({})", activation());

        let worklet: fn() -> Worklet = xila_audio_worklet;
        let _ = worklet;
        let configuration = nanoaudio_web::Configuration::new(48_000, 2, Format::F32);
        let output = match within(
            OPEN_TIMEOUT_MS,
            Output::open_with(configuration, "xila_audio_worklet", Options::default()),
        )
        .await
        {
            Some(Ok(output)) => output,
            Some(Err(error)) => {
                let error = StartError::Open(describe(&error));
                log::error!("Browser audio: {error}");
                return Err(error);
            }
            None => {
                log::error!("Browser audio: {}", StartError::OpenTimedOut);
                return Err(StartError::OpenTimedOut);
            }
        };
        log::information!(
            "Browser audio: worklet loaded (context state {:?}, {} Hz, {})",
            output.context().state(),
            output.device_sample_rate(),
            activation()
        );

        match within(RESUME_TIMEOUT_MS, output.play()).await {
            Some(Ok(())) => log::information!(
                "Browser audio: context resumed (state {:?})",
                output.context().state()
            ),
            Some(Err(error)) => {
                let error = StartError::Resume(describe(&error));
                log::error!("Browser audio: {error}");
                return Err(error);
            }
            None => {
                log::error!(
                    "Browser audio: {} (state {:?}, {})",
                    StartError::ResumeTimedOut,
                    output.context().state(),
                    activation()
                );
                return Err(StartError::ResumeTimedOut);
            }
        }

        let bridge = Box::new(Bridge {
            slots: array::from_fn(|_| BridgeSlot::new()),
            next_id: AtomicU32::new(0),
            pending_controls: AtomicUsize::new(0),
            rejected_controls: AtomicU32::new(0),
            output,
        });
        let bridge_ptr = (&*bridge) as *const Bridge;
        bridge.output.set_on_message(move |value| {
            let bridge = unsafe { &*bridge_ptr };
            bridge.on_message(value);
        });

        startup.commit();
        Ok(Self { bridge })
    }

    fn stream<'a>(&self, context: &'a Context) -> Result<&'a StreamProxy> {
        context
            .get_private_data_of_type::<StreamProxy>()
            .ok_or(Kind::InvalidState)
    }

    fn post_open(&self, id: u32) -> Result<()> {
        let packet = packet("open", id);
        self.bridge.send(&packet, &[])
    }

    fn post_close(&self, slot: usize, id: u32) -> Result<()> {
        self.bridge.slots[slot]
            .closing
            .store(true, Ordering::Release);
        let packet = packet("close", id);
        if let Err(error) = self.bridge.send(&packet, &[]) {
            self.bridge.release(slot, id);
            return Err(error);
        }
        Ok(())
    }
}

impl Drop for WebAudioDevice {
    fn drop(&mut self) {
        STARTED.store(false, Ordering::Release);
    }
}

struct StreamProxy {
    slot: usize,
    id: u32,
    configuration: Configuration,
    volume: u8,
}

impl BaseOperations for WebAudioDevice {
    fn open(&self, context: &mut Context) -> Result<()> {
        if context.get_private_data().is_some() {
            return Err(Kind::InvalidState);
        }
        let (slot, id) = self.bridge.allocate()?;
        if let Err(error) = self.post_open(id) {
            self.bridge.release(slot, id);
            return Err(error);
        }
        context.set_private_data(Box::new(StreamProxy {
            slot,
            id,
            configuration: OUTPUT_CONFIGURATION,
            volume: 100,
        }));
        Ok(())
    }

    fn close(&self, context: &mut Context) -> Result<()> {
        let proxy = context
            .take_private_data_of_type::<StreamProxy>()
            .ok_or(Kind::InvalidState)?;
        self.post_close(proxy.slot, proxy.id)
    }

    fn read(
        &self,
        _context: &mut Context,
        _cx: &mut core::task::Context<'_>,
        _buffer: &mut [u8],
        _position: Size,
    ) -> core::task::Poll<Result<usize>> {
        file_system::ready(|| Err(Kind::Unsupported))
    }

    fn write(
        &self,
        context: &mut Context,
        _cx: &mut core::task::Context<'_>,
        bytes: &[u8],
        _position: Size,
    ) -> core::task::Poll<Result<usize>> {
        file_system::ready(|| {
            let proxy = self.stream(context)?;
            if !bytes.len().is_multiple_of(FRAME_BYTES) {
                return Err(Kind::InvalidArgument);
            }
            if bytes.is_empty() {
                return Ok(0);
            }
            let slot = &self.bridge.slots[proxy.slot];
            let count =
                slot.reserve(bytes.len().min(MAX_TRANSFER_BYTES) / FRAME_BYTES * FRAME_BYTES);
            if count == 0 {
                return Err(Kind::WouldBlock);
            }
            let (data, buffer) = make_bytes(&bytes[..count]);
            let packet = packet("write", proxy.id);
            if set(&packet, "bytes", data.as_ref()).is_err()
                || self.bridge.send(&packet, &[buffer]).is_err()
            {
                slot.undo_reservation(count);
                return Err(Kind::InputOutput);
            }
            Ok(count)
        })
    }

    fn flush(
        &self,
        context: &mut Context,
        _cx: &mut core::task::Context<'_>,
    ) -> core::task::Poll<Result<()>> {
        file_system::ready(|| {
            let proxy = self.stream(context)?;
            let slot = &self.bridge.slots[proxy.slot];
            if slot.writable() == STREAM_BYTES {
                Ok(())
            } else {
                Err(Kind::WouldBlock)
            }
        })
    }

    fn control(
        &self,
        context: &mut Context,
        _cx: &mut core::task::Context<'_>,
        command: ControlCommandIdentifier,
        input: &AnyByLayout,
        output: &mut AnyByLayout,
    ) -> core::task::Poll<Result<()>> {
        file_system::ready(|| {
            let proxy = context
                .get_private_data_mutable_of_type::<StreamProxy>()
                .ok_or(Kind::InvalidState)?;
            if input.get_size() > MAX_CONTROL_BYTES || output.get_size() > MAX_CONTROL_BYTES {
                return Err(Kind::InvalidArgument);
            }
            let slot = &self.bridge.slots[proxy.slot];
            match command {
                SET_CONFIGURATION::IDENTIFIER => {
                    validate::<SET_CONFIGURATION>(input, output)?;
                    let value = *SET_CONFIGURATION::cast_input(input)?;
                    value.validate()?;
                    if value != OUTPUT_CONFIGURATION {
                        return Err(Kind::Unsupported);
                    }
                    self.bridge.send_control(
                        proxy.id,
                        command,
                        any_bytes(input),
                        output.get_size(),
                    )?;
                    proxy.configuration = value;
                }
                GET_CONFIGURATION::IDENTIFIER => {
                    validate::<GET_CONFIGURATION>(input, output)?;
                    *GET_CONFIGURATION::cast_output(output)? = proxy.configuration;
                    self.bridge.send_control(
                        proxy.id,
                        command,
                        any_bytes(input),
                        output.get_size(),
                    )?;
                }
                GET_WRITABLE::IDENTIFIER => {
                    validate::<GET_WRITABLE>(input, output)?;
                    *GET_WRITABLE::cast_output(output)? =
                        slot.writable().min(u32::MAX as usize) as u32;
                    self.bridge.send_control(
                        proxy.id,
                        command,
                        any_bytes(input),
                        output.get_size(),
                    )?;
                }
                SET_VOLUME::IDENTIFIER => {
                    validate::<SET_VOLUME>(input, output)?;
                    let value = *SET_VOLUME::cast_input(input)?;
                    if value > 100 {
                        return Err(Kind::InvalidArgument);
                    }
                    self.bridge.send_control(
                        proxy.id,
                        command,
                        any_bytes(input),
                        output.get_size(),
                    )?;
                    proxy.volume = value;
                }
                GET_VOLUME::IDENTIFIER => {
                    validate::<GET_VOLUME>(input, output)?;
                    *GET_VOLUME::cast_output(output)? = proxy.volume;
                    self.bridge.send_control(
                        proxy.id,
                        command,
                        any_bytes(input),
                        output.get_size(),
                    )?;
                }
                START::IDENTIFIER => {
                    validate::<START>(input, output)?;
                    self.bridge.send_control(
                        proxy.id,
                        command,
                        any_bytes(input),
                        output.get_size(),
                    )?;
                }
                STOP::IDENTIFIER => {
                    validate::<STOP>(input, output)?;
                    self.bridge.send_control(
                        proxy.id,
                        command,
                        any_bytes(input),
                        output.get_size(),
                    )?;
                }
                _ => return Err(Kind::Unsupported),
            }
            Ok(())
        })
    }

    fn clone_context(&self, _context: &Context) -> Result<Context> {
        Err(Kind::Unsupported)
    }
}

impl MountOperations for WebAudioDevice {}
impl CharacterDevice for WebAudioDevice {}

struct WorkletSource {
    system: Pin<&'static AudioSystem<STREAMS, STREAM_BYTES, MIX_FRAMES, OUTPUTS, SINK_BYTES>>,
    output: OutputHandle<'static, STREAMS, STREAM_BYTES, MIX_FRAMES, OUTPUTS, SINK_BYTES>,
    streams: [Option<StreamHandle<'static, STREAMS, STREAM_BYTES, MIX_FRAMES, OUTPUTS, SINK_BYTES>>;
        STREAMS],
    ids: [u32; STREAMS],
    accepted: [u64; STREAMS],
    reported: [u64; STREAMS],
    scratch: [u8; MIX_BLOCK_BYTES],
    control_input: AlignedControl,
    control_output: AlignedControl,
}

#[repr(align(16))]
struct AlignedControl([u8; MAX_CONTROL_BYTES]);

impl WorkletSource {
    fn new() -> Self {
        let system: &'static AudioSystem<STREAMS, STREAM_BYTES, MIX_FRAMES, OUTPUTS, SINK_BYTES> =
            Box::leak(Box::new(
                AudioSystem::<STREAMS, STREAM_BYTES, MIX_FRAMES, OUTPUTS, SINK_BYTES>::new()
                    .expect("valid browser audio capacities"),
            ));
        let system = unsafe { Pin::new_unchecked(system) };
        let output = system
            .open_output()
            .expect("browser output capacity is available");
        Self {
            system,
            output,
            streams: array::from_fn(|_| None),
            ids: [0; STREAMS],
            accepted: [0; STREAMS],
            reported: [0; STREAMS],
            scratch: [0; MIX_BLOCK_BYTES],
            control_input: AlignedControl([0; MAX_CONTROL_BYTES]),
            control_output: AlignedControl([0; MAX_CONTROL_BYTES]),
        }
    }

    fn send_open_ack(&self, id: u32, port: &Port) {
        let packet = packet("open_ack", id);
        let _ = port.post(&packet.into());
    }

    fn dispatch(&mut self, value: JsValue, port: &Port) {
        let Ok(op) = field(&value, "op") else {
            return;
        };
        let Ok(id) = number_field(&value, "id") else {
            return;
        };
        match op.as_string().as_deref() {
            Some("open") => {
                if self.ids.contains(&id) || id == 0 {
                    return;
                }
                let Some(index) = self.ids.iter().position(|current| *current == 0) else {
                    return;
                };
                if let Ok(stream) = self.system.get_ref().open_stream() {
                    self.streams[index] = Some(stream);
                    self.ids[index] = id;
                    self.accepted[index] = 0;
                    self.reported[index] = 0;
                    self.send_open_ack(id, port);
                }
            }
            Some("write") => {
                let Some(index) = self.ids.iter().position(|current| *current == id) else {
                    return;
                };
                let Ok(bytes_value) = field(&value, "bytes") else {
                    return;
                };
                let Ok(bytes) = bounded_bytes(bytes_value, MAX_TRANSFER_BYTES) else {
                    return;
                };
                let length = bytes.length() as usize;
                if !length.is_multiple_of(FRAME_BYTES) {
                    return;
                }
                bytes.copy_to(&mut self.scratch[..length]);
                let Some(stream) = self.streams[index].as_ref() else {
                    return;
                };
                if let Ok(count) = stream.write(&self.scratch[..length]) {
                    self.accepted[index] = self.accepted[index].saturating_add(count as u64);
                }
            }
            Some("close") => {
                if let Some(index) = self.ids.iter().position(|current| *current == id) {
                    self.streams[index] = None;
                    self.ids[index] = 0;
                    self.accepted[index] = 0;
                    self.reported[index] = 0;
                    let packet = packet("close_ack", id);
                    let _ = port.post(&packet.into());
                }
            }
            Some("control") => self.control(&value, id, port),
            _ => {}
        }
    }

    fn control(&mut self, value: &JsValue, id: u32, port: &Port) {
        let Some(index) = self.ids.iter().position(|current| *current == id) else {
            return;
        };
        let Ok(command_raw) = number_field(value, "command") else {
            return;
        };
        let Ok(output_size) = number_field(value, "outputSize") else {
            return;
        };
        let Ok(bytes_value) = field(value, "bytes") else {
            return;
        };
        let Ok(bytes) = bounded_bytes(bytes_value, MAX_CONTROL_BYTES) else {
            return;
        };
        let mut input = [0; MAX_CONTROL_BYTES];
        let input_size = bytes.length() as usize;
        bytes.copy_to(&mut input[..input_size]);
        let result = self.run_control(
            index,
            ControlCommandIdentifier::from_raw(command_raw),
            &input[..input_size],
            output_size as usize,
        );
        let packet = packet("control_ack", id);
        let _ = set(&packet, "ok", &JsValue::from_bool(result.is_ok()));
        let _ = set(&packet, "command", &JsValue::from_f64(command_raw as f64));
        if let Err(error) = result {
            let _ = set(&packet, "error", &JsValue::from_str(&format!("{error:?}")));
        }
        let _ = port.post(&packet.into());
    }

    /// Runs one control command on the stream in slot `index`, with `input` as the command's input bytes and `output_size` zeroed bytes of output left in `control_output`.
    ///
    /// Each command casts only the buffers it uses: the cast of a zero-sized input or output fails by design, and most commands have one.
    fn run_control(
        &mut self,
        index: usize,
        command: ControlCommandIdentifier,
        input: &[u8],
        output_size: usize,
    ) -> Result<()> {
        if input.len() > MAX_CONTROL_BYTES || output_size > MAX_CONTROL_BYTES {
            return Err(Kind::InvalidArgument);
        }
        self.control_input.0[..input.len()].copy_from_slice(input);
        self.control_output.0[..output_size].fill(0);
        let input_size = input.len();
        let input =
            unsafe { AnyByLayout::from_raw_parts(self.control_input.0.as_mut_ptr(), input_size) };
        let output =
            unsafe { AnyByLayout::from_raw_parts(self.control_output.0.as_mut_ptr(), output_size) };
        let stream = &self.streams[index];
        match command {
            SET_CONFIGURATION::IDENTIFIER => {
                apply::<SET_CONFIGURATION>(stream, input, output, |stream, input, _| {
                    let config = SET_CONFIGURATION::cast_input(input)?;
                    config.validate()?;
                    if *config != OUTPUT_CONFIGURATION {
                        return Err(Kind::Unsupported);
                    }
                    let _ = stream.configuration();
                    Ok(())
                })
            }
            GET_CONFIGURATION::IDENTIFIER => {
                apply::<GET_CONFIGURATION>(stream, input, output, |stream, _, output| {
                    *GET_CONFIGURATION::cast_output(output)? = stream.configuration();
                    Ok(())
                })
            }
            GET_WRITABLE::IDENTIFIER => {
                apply::<GET_WRITABLE>(stream, input, output, |stream, _, output| {
                    *GET_WRITABLE::cast_output(output)? =
                        stream.writable().min(u32::MAX as usize) as u32;
                    Ok(())
                })
            }
            SET_VOLUME::IDENTIFIER => {
                apply::<SET_VOLUME>(stream, input, output, |stream, input, _| {
                    stream.set_volume(*SET_VOLUME::cast_input(input)?)
                })
            }
            GET_VOLUME::IDENTIFIER => {
                apply::<GET_VOLUME>(stream, input, output, |stream, _, output| {
                    *GET_VOLUME::cast_output(output)? = stream.volume()?;
                    Ok(())
                })
            }
            START::IDENTIFIER => {
                apply::<START>(stream, input, output, |stream, _, _| stream.start())
            }
            STOP::IDENTIFIER => apply::<STOP>(stream, input, output, |stream, _, _| stream.stop()),
            _ => Err(Kind::Unsupported),
        }
    }

    /// Mixes the next block and moves up to `free` bytes of it into `scratch`; returns the byte count.
    fn render(&mut self, free: usize) -> usize {
        let _ = self.system.as_ref().mix_next_block();
        self.output
            .with_consumer(|consumer| consumer.read(&mut self.scratch[..free.min(MIX_BLOCK_BYTES)]))
    }

    fn report_consumed(&mut self, port: &Port) {
        for index in 0..STREAMS {
            let Some(stream) = self.streams[index].as_ref() else {
                continue;
            };
            let queued = STREAM_BYTES.saturating_sub(stream.writable());
            let consumed = self.accepted[index].saturating_sub(queued as u64);
            if consumed > self.reported[index] {
                self.reported[index] = consumed;
                let packet = packet("consumed", self.ids[index]);
                let _ = set(&packet, "bytes", &JsValue::from_f64(consumed as f64));
                let _ = port.post(&packet.into());
            }
        }
    }
}

impl Source for WorkletSource {
    fn fill(
        &mut self,
        producer: &mut nanoaudio_web::Producer<'_, { nanoaudio_web::CAPACITY }>,
        port: &Port,
    ) {
        let count = self.render(producer.free());
        producer.write(&self.scratch[..count]);
        self.report_consumed(port);
    }

    fn message(&mut self, data: JsValue, port: &Port) {
        self.dispatch(data, port);
    }
}

#[wasm_bindgen]
pub fn xila_audio_worklet() -> Worklet {
    Worklet::output(WorkletSource::new())
}

fn apply<C: ControlCommand>(
    stream: &Option<StreamHandle<'static, STREAMS, STREAM_BYTES, MIX_FRAMES, OUTPUTS, SINK_BYTES>>,
    input: &AnyByLayout,
    output: &mut AnyByLayout,
    operation: impl FnOnce(
        &StreamHandle<'static, STREAMS, STREAM_BYTES, MIX_FRAMES, OUTPUTS, SINK_BYTES>,
        &AnyByLayout,
        &mut AnyByLayout,
    ) -> Result<()>,
) -> Result<()> {
    validate::<C>(input, output)?;
    let stream = stream.as_ref().ok_or(Kind::InvalidState)?;
    operation(stream, input, output)
}

fn validate<C: ControlCommand>(input: &AnyByLayout, output: &AnyByLayout) -> Result<()> {
    if input.get_size() != size_of::<C::Input>()
        || output.get_size() != size_of::<C::Output>()
        || !(core::ptr::from_ref(input).cast::<u8>() as usize)
            .is_multiple_of(core::mem::align_of::<C::Input>())
        || !(core::ptr::from_ref(output).cast::<u8>() as usize)
            .is_multiple_of(core::mem::align_of::<C::Output>())
    {
        return Err(Kind::InvalidArgument);
    }
    Ok(())
}

/// Describes a `control_ack` that reports a failure, or returns `None` for a successful or unrelated message.
fn control_rejection(value: &JsValue) -> Option<String> {
    if field(value, "op").ok()?.as_string().as_deref() != Some("control_ack")
        || field(value, "ok").ok()?.as_bool() != Some(false)
    {
        return None;
    }
    let text = |key: &str| field(value, key).ok().and_then(|field| field.as_string());
    let command = number_field(value, "command").ok();
    let stream = number_field(value, "id").ok();
    Some(format!(
        "control command {command:?} on stream {stream:?}: {}",
        text("error").unwrap_or_else(|| "no error reported".to_string())
    ))
}

fn set(object: &Object, key: &str, value: &JsValue) -> Result<()> {
    Reflect::set(object, &JsValue::from_str(key), value)
        .map(|_| ())
        .map_err(|_| Kind::InputOutput)
}

fn field(value: &JsValue, key: &str) -> core::result::Result<JsValue, JsValue> {
    Reflect::get(value, &JsValue::from_str(key))
}

fn number_field(value: &JsValue, key: &str) -> core::result::Result<u32, JsValue> {
    let number = field(value, key)?
        .as_f64()
        .ok_or_else(|| JsValue::from_str("invalid number"))?;
    if !number.is_finite() || number < 0.0 || number > u32::MAX as f64 || number.fract() != 0.0 {
        return Err(JsValue::from_str("invalid number"));
    }
    Ok(number as u32)
}

fn number_field64(value: &JsValue, key: &str) -> core::result::Result<u64, JsValue> {
    let number = field(value, key)?
        .as_f64()
        .ok_or_else(|| JsValue::from_str("invalid number"))?;
    if !number.is_finite() || number < 0.0 || number > u64::MAX as f64 || number.fract() != 0.0 {
        return Err(JsValue::from_str("invalid number"));
    }
    Ok(number as u64)
}

fn packet(op: &str, id: u32) -> Object {
    let packet = Object::new();
    let _ = Reflect::set(&packet, &"op".into(), &op.into());
    let _ = Reflect::set(&packet, &"id".into(), &JsValue::from_f64(id as f64));
    packet
}

fn make_bytes(bytes: &[u8]) -> (Uint8Array, ArrayBuffer) {
    let array = Uint8Array::new_with_length(bytes.len() as u32);
    array.copy_from(bytes);
    let buffer = array.buffer();
    (array, buffer)
}

fn bounded_bytes(value: JsValue, maximum: usize) -> core::result::Result<Uint8Array, JsValue> {
    let bytes = value.dyn_into::<Uint8Array>()?;
    if bytes.length() as usize > maximum {
        return Err(JsValue::from_str("audio message exceeds its size limit"));
    }
    Ok(bytes)
}

fn any_bytes(value: &AnyByLayout) -> &[u8] {
    unsafe {
        core::slice::from_raw_parts((value as *const AnyByLayout).cast::<u8>(), value.get_size())
    }
}

#[cfg(test)]
mod tests {
    use super::{
        BridgeSlot, ControlCommand, FRAME_BYTES, Kind, MAX_SUMMARY_CHARACTERS,
        OUTPUT_CONFIGURATION, STREAM_BYTES, StartError, StartGuard, WorkletSource,
        control_rejection, set, shorten,
    };
    use alloc::string::ToString;
    use audio::{
        Configuration, GET_CONFIGURATION, GET_VOLUME, GET_WRITABLE, SET_CONFIGURATION, SET_VOLUME,
        START, STOP, SampleFormat,
    };
    use core::sync::atomic::{AtomicBool, Ordering};
    use js_sys::Object;
    use wasm_bindgen::JsValue;
    use wasm_bindgen_test::wasm_bindgen_test;

    #[wasm_bindgen_test]
    fn shortens_long_errors_on_a_character_boundary() {
        let long = "é".repeat(MAX_SUMMARY_CHARACTERS + 10);
        let short = shorten(&long);
        assert_eq!(short.chars().count(), MAX_SUMMARY_CHARACTERS + 1);
        assert!(short.ends_with('…'));
        assert_eq!(shorten("short"), "short");
    }

    #[wasm_bindgen_test]
    fn summaries_carry_the_browser_error_and_the_autoplay_hint() {
        assert!(
            StartError::Open("AbortError: The operation was aborted.".to_string())
                .summary()
                .contains("AbortError")
        );
        assert!(
            StartError::Resume("NotAllowedError: blocked".to_string())
                .summary()
                .contains("NotAllowedError")
        );
        assert!(StartError::ResumeTimedOut.summary().contains("allow audio"));
    }

    #[wasm_bindgen_test]
    fn uncommitted_start_guard_resets_reservation() {
        let started = AtomicBool::new(false);
        let guard = StartGuard::acquire(&started).unwrap();
        assert!(started.load(Ordering::Acquire));
        drop(guard);
        assert!(!started.load(Ordering::Acquire));
    }

    #[wasm_bindgen_test]
    fn committed_start_guard_keeps_reservation() {
        let started = AtomicBool::new(false);
        StartGuard::acquire(&started).unwrap().commit();
        assert!(started.load(Ordering::Acquire));
    }

    #[wasm_bindgen_test]
    fn bounds_outstanding_pcm_and_restores_only_consumed_credit() {
        let credit = BridgeSlot::new();
        credit
            .ready
            .store(true, core::sync::atomic::Ordering::Release);

        assert_eq!(credit.writable(), 8192);
        assert_eq!(credit.reserve(STREAM_BYTES - 512), STREAM_BYTES - 512);
        assert_eq!(credit.writable(), 512);
        credit.report_consumed(512);
        assert_eq!(credit.writable(), 1024);
        assert_eq!(credit.reserve(STREAM_BYTES), 1024);
        assert_eq!(credit.writable(), 0);
    }

    #[wasm_bindgen_test]
    fn ignores_old_consumption_reports() {
        let credit = BridgeSlot::new();
        credit
            .ready
            .store(true, core::sync::atomic::Ordering::Release);

        credit.reserve(1024);
        credit.report_consumed(512);
        credit.report_consumed(256);

        assert_eq!(credit.writable(), STREAM_BYTES - 512);
    }

    /// A worklet source with one open stream in slot 0, driven through the same `run_control` path as a control message from the page.
    fn source_with_stream() -> WorkletSource {
        let mut source = WorkletSource::new();
        let stream = source
            .system
            .get_ref()
            .open_stream()
            .expect("a free stream");
        source.streams[0] = Some(stream);
        source.ids[0] = 1;
        source
    }

    fn bytes_of<T>(value: &T) -> &[u8] {
        unsafe { core::slice::from_raw_parts((value as *const T).cast(), size_of::<T>()) }
    }

    /// Dispatches `C` as the page does: its input bytes and the output size of its output type.
    fn dispatch<C: ControlCommand>(
        source: &mut WorkletSource,
        input: &[u8],
    ) -> Result<C::Output, Kind> {
        dispatch_sized::<C>(source, input, size_of::<C::Output>())
    }

    fn dispatch_sized<C: ControlCommand>(
        source: &mut WorkletSource,
        input: &[u8],
        output_size: usize,
    ) -> Result<C::Output, Kind> {
        source.run_control(0, C::IDENTIFIER, input, output_size)?;
        Ok(unsafe {
            core::ptr::read_unaligned(source.control_output.0.as_ptr().cast::<C::Output>())
        })
    }

    /// One block of constant stereo F32 frames, 0.25 on both channels.
    fn one_block() -> alloc::vec::Vec<u8> {
        let frame = [0.25f32, 0.25f32];
        (0..super::MIX_FRAMES)
            .flat_map(|_| bytes_of(&frame).to_vec())
            .collect()
    }

    // Every audio command has `()` as its input or its output, and casting a zero-sized buffer fails: the commands must succeed all the same.
    #[wasm_bindgen_test]
    fn commands_without_input_or_output_succeed() {
        let mut source = source_with_stream();
        dispatch::<START>(&mut source, &[]).expect("START");
        dispatch::<STOP>(&mut source, &[]).expect("STOP");
        dispatch::<SET_VOLUME>(&mut source, &[40]).expect("SET_VOLUME");
        dispatch::<SET_CONFIGURATION>(&mut source, bytes_of(&OUTPUT_CONFIGURATION))
            .expect("SET_CONFIGURATION");
    }

    #[wasm_bindgen_test]
    fn commands_return_their_output() {
        let mut source = source_with_stream();
        dispatch::<SET_VOLUME>(&mut source, &[40]).unwrap();
        assert_eq!(dispatch::<GET_VOLUME>(&mut source, &[]).unwrap(), 40);
        assert_eq!(
            dispatch::<GET_WRITABLE>(&mut source, &[]).unwrap(),
            STREAM_BYTES as u32
        );
        let configuration = dispatch::<GET_CONFIGURATION>(&mut source, &[]).unwrap();
        assert_eq!(configuration, OUTPUT_CONFIGURATION);
    }

    #[wasm_bindgen_test]
    fn rejects_bad_requests_with_the_matching_error() {
        let mut source = source_with_stream();
        let other = Configuration::new(44_100, 2, SampleFormat::F32);
        assert_eq!(
            dispatch::<SET_CONFIGURATION>(&mut source, bytes_of(&other)).unwrap_err(),
            Kind::Unsupported
        );
        assert_eq!(
            dispatch::<SET_VOLUME>(&mut source, &[101]).unwrap_err(),
            Kind::InvalidArgument
        );
        assert_eq!(
            dispatch::<START>(&mut source, &[1]).unwrap_err(),
            Kind::InvalidArgument
        );
        assert_eq!(
            dispatch_sized::<GET_VOLUME>(&mut source, &[], 0).unwrap_err(),
            Kind::InvalidArgument
        );
        assert_eq!(
            source
                .run_control(0, super::ControlCommandIdentifier::from_raw(0), &[], 0)
                .unwrap_err(),
            Kind::Unsupported
        );
        assert_eq!(
            source
                .run_control(1, START::IDENTIFIER, &[], 0)
                .unwrap_err(),
            Kind::InvalidState
        );
    }

    // The stream only plays after START: this is what made the browser silent.
    #[wasm_bindgen_test]
    fn start_makes_the_worklet_mix_the_stream_and_stop_halts_it() {
        let mut source = source_with_stream();
        let block = one_block();
        let stream = source.streams[0].as_ref().unwrap();
        assert_eq!(stream.write(&block).unwrap(), block.len());
        assert_eq!(stream.writable(), STREAM_BYTES - block.len());

        let count = source.render(usize::MAX);
        assert!(source.scratch[..count].iter().all(|byte| *byte == 0));
        assert_eq!(
            dispatch::<GET_WRITABLE>(&mut source, &[]).unwrap() as usize,
            STREAM_BYTES - block.len(),
            "a stream that was never started keeps its queue"
        );

        dispatch::<START>(&mut source, &[]).unwrap();
        let count = source.render(usize::MAX);
        assert_eq!(count, block.len());
        let sample = f32::from_ne_bytes(source.scratch[..4].try_into().unwrap());
        assert!(sample > 0.0, "the started stream is mixed, got {sample}");
        assert_eq!(
            dispatch::<GET_WRITABLE>(&mut source, &[]).unwrap(),
            STREAM_BYTES as u32,
            "the mixer drained the stream"
        );

        dispatch::<STOP>(&mut source, &[]).unwrap();
        source.streams[0].as_ref().unwrap().write(&block).unwrap();
        let count = source.render(usize::MAX);
        assert!(source.scratch[..count].iter().all(|byte| *byte == 0));
        assert_eq!(FRAME_BYTES * super::MIX_FRAMES, block.len());
    }

    fn ack(ok: bool, error: Option<&str>) -> JsValue {
        let packet = Object::new();
        set(&packet, "op", &JsValue::from_str("control_ack")).unwrap();
        set(&packet, "id", &JsValue::from_f64(3.0)).unwrap();
        set(&packet, "ok", &JsValue::from_bool(ok)).unwrap();
        set(&packet, "command", &JsValue::from_f64(1_074_282_754.0)).unwrap();
        if let Some(error) = error {
            set(&packet, "error", &JsValue::from_str(error)).unwrap();
        }
        packet.into()
    }

    #[wasm_bindgen_test]
    fn only_a_failed_control_ack_is_reported() {
        assert_eq!(control_rejection(&ack(true, None)), None);
        let rejection = control_rejection(&ack(false, Some("InvalidParameter"))).unwrap();
        assert!(rejection.contains("1074282754"), "{rejection}");
        assert!(rejection.contains("stream Some(3)"), "{rejection}");
        assert!(rejection.contains("InvalidParameter"), "{rejection}");
        let other = Object::new();
        set(&other, "op", &JsValue::from_str("consumed")).unwrap();
        set(&other, "ok", &JsValue::from_bool(false)).unwrap();
        assert_eq!(control_rejection(&other.into()), None);
    }
}
