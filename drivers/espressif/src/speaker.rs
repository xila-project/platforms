//! PCM playback through I2S transmit DMA on the ESP32-S3.

use alloc::boxed::Box;
use core::{
    pin::Pin,
    sync::atomic::{AtomicBool, AtomicU32, Ordering},
    task::{Context as TaskContext, Poll},
};

use audio::{AudioStreamDevice, AudioSystem, OutputHandle};
use device::kind;
use embassy_time::{Duration, Instant, Timer, with_timeout};
use esp_hal::{
    Async,
    dma::{DmaDescriptor, DmaTxStreamBuf},
    i2s::master::{ConfigError, I2sTx, I2sTxDmaTransfer, TdmConfig},
    time::Rate,
};
use file_system::{
    BaseOperations, CharacterDevice, Context, ControlCommand, ControlCommandIdentifier,
    IntoIoErrorKind, IoErrorKind, MountOperations, Result, Size, define_command,
};
use shared::AnyByLayout;
use task::{Manager, TaskIdentifier};
use virtual_file_system::VirtualFileSystem;

const SAMPLE_RATE: u32 = 48_000;
const FRAMES: usize = 256;
const MIX_FRAME_BYTES: usize = 2 * size_of::<f32>();
const MIX_BYTES: usize = FRAMES * MIX_FRAME_BYTES;
const TRANSMIT_BYTES: usize = FRAMES * 2 * size_of::<i16>();
const DESCRIPTOR_COUNT: usize = 8;
const DMA_BUFFER_BYTES: usize = DESCRIPTOR_COUNT * TRANSMIT_BYTES;

/// Internal SRAM reserved for DMA. Keep this outside the firmware heap budget.
pub const DMA_STORAGE_BYTES: usize =
    DMA_BUFFER_BYTES + DESCRIPTOR_COUNT * size_of::<DmaDescriptor>();
const STREAMS: usize = 4;
const STREAM_BYTES: usize = 4096;
const PLAYBACK_PATH: &str = "/devices/audio/playback";
/// How long the refill waits for the next descriptor end-of-frame interrupt
/// before treating the transfer as stalled.
const REFILL_TIMEOUT: Duration = Duration::from_millis(100);
/// Consecutive failed restarts after which the device reports `DeviceUnavailable`.
const MAX_RESTART_ATTEMPTS: usize = 8;
/// Underrun log lines are limited to one per interval.
const LOG_INTERVAL: Duration = Duration::from_secs(1);

/// Platform extension of the audio device kind: total recovered DMA underruns.
define_command!(GET_UNDERRUNS, Read, kind::AUDIO, 0x08, (), u32);

type System = AudioSystem<STREAMS, STREAM_BYTES, FRAMES, 1, MIX_BYTES>;
type Output = OutputHandle<'static, STREAMS, STREAM_BYTES, FRAMES, 1, MIX_BYTES>;
type StreamDevice = AudioStreamDevice<STREAMS, STREAM_BYTES, FRAMES, 1, MIX_BYTES>;
type Transfer = I2sTxDmaTransfer<'static, Async, DmaTxStreamBuf>;

/// The board transmitter or the error returned when configuring it.
pub type Transmitter = core::result::Result<I2sTx<'static, Async>, ConfigError>;

/// The on-wire format for the amplifier.
pub fn configuration() -> TdmConfig {
    TdmConfig::new_tdm_philips().with_sample_rate(Rate::from_hz(SAMPLE_RATE))
}

/// State shared between the playback device and the DMA task.
struct State {
    failed: AtomicBool,
    underruns: AtomicU32,
}

impl State {
    const fn new() -> Self {
        Self {
            failed: AtomicBool::new(false),
            underruns: AtomicU32::new(0),
        }
    }

    /// Increments the underrun count, saturating at the payload width, and
    /// returns the new value.
    fn count_underrun(&self) -> u32 {
        self.underruns
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |count| {
                count.checked_add(1)
            })
            .map(|count| count + 1)
            .unwrap_or(u32::MAX)
    }
}

/// Mounts the PCM device and starts its DMA task. A DMA underrun is recovered
/// by the task; only a failure that a restart cannot fix, such as a restart
/// that keeps failing or a mixer error, reports an I/O error, including to
/// clients with an open stream.
pub async fn mount(
    transmitter: Transmitter,
    file_system: &'static VirtualFileSystem,
    tasks: &'static Manager,
    task: TaskIdentifier,
) -> Result<()> {
    let system = Box::leak(Box::new(System::new()?));
    // SAFETY: the leaked mixer stays at this address for the life of the firmware.
    let system = unsafe { Pin::new_unchecked(&*system) };
    let output = system.open_output()?;
    static STATE: State = State::new();
    file_system
        .mount_character_device(
            task,
            PLAYBACK_PATH,
            Playback {
                device: StreamDevice::new(system),
                state: &STATE,
            },
        )
        .await?;

    let transmitter = transmitter.map_err(|error| {
        STATE.failed.store(true, Ordering::Release);
        log::error!("I2S configuration failed: {error:?}");
        IoErrorKind::DeviceUnavailable
    })?;
    tasks
        .spawn(task, "Speaker output", None, async move |_| {
            if let Err(error) = transmit(transmitter, system, output, &STATE).await {
                STATE.failed.store(true, Ordering::Release);
                log::error!("I2S playback failed: {error}");
            }
        })
        .await
        .map_err(|error| {
            STATE.failed.store(true, Ordering::Release);
            error.into_io_error_kind()
        })?;
    Ok(())
}

async fn transmit(
    transmitter: I2sTx<'static, Async>,
    system: Pin<&'static System>,
    mut output: Output,
    state: &'static State,
) -> core::result::Result<(), &'static str> {
    // The static DMA buffer is in internal memory. Eight descriptors hold
    // 42.7 ms of audio while other tasks run. Start with silence in each one.
    let buffer: DmaTxStreamBuf = esp_hal::dma_tx_stream_buffer!(DMA_BUFFER_BYTES, TRANSMIT_BYTES);
    let mut transfer = transmitter
        .write(buffer)
        .map_err(|_| "could not start transmit DMA")?;
    let mut mixed = [0; MIX_BYTES];
    let mut last_underrun_log: Option<Instant> = None;

    loop {
        let waited = with_timeout(REFILL_TIMEOUT, transfer.wait_for_available_async()).await;
        match waited {
            Ok(Ok(())) => {
                // A descriptor ended: refill every whole block the DMA released.
                let available_blocks = transfer.available_bytes() / TRANSMIT_BYTES;
                for _ in 0..available_blocks {
                    mix_block(system, &mut output, &mut mixed)?;
                    transfer.push_with(|block| {
                        encode_block(&mut block[..TRANSMIT_BYTES], &mixed);
                        TRANSMIT_BYTES
                    });
                }
            }
            // The DMA consumed every descriptor (TotalEof) or the refill ran
            // late: recover instead of failing the device.
            Ok(Err(_)) | Err(_) => {
                let underruns = state.count_underrun();
                let now = Instant::now();
                if last_underrun_log.is_none_or(|last| now - last >= LOG_INTERVAL) {
                    log::warning!("I2S DMA underrun count {underruns}; the transfer was restarted");
                    last_underrun_log = Some(now);
                }
                transfer = restart(transfer, system, &mut output, &mut mixed).await?;
            }
        }
    }
}

/// Stops a stalled transfer, refills the whole DMA buffer with silence plus the
/// next mixed block and restarts the transfer. Fails only when the restart
/// itself keeps failing, which a larger buffer would not fix.
async fn restart(
    transfer: Transfer,
    system: Pin<&'static System>,
    output: &mut Output,
    mixed: &mut [u8; MIX_BYTES],
) -> core::result::Result<Transfer, &'static str> {
    let (mut transmitter, buffer) = transfer.stop();
    let (descriptors, mut storage) = buffer.split();
    // Silence everywhere, then the next mixed block in the first descriptor.
    storage.fill(0);
    mix_block(system, output, mixed)?;
    encode_block(&mut storage[..TRANSMIT_BYTES], mixed);
    let mut buffer = DmaTxStreamBuf::new(descriptors, storage)
        .map_err(|_| "could not rebuild the DMA stream buffer")?;
    for attempt in 1..=MAX_RESTART_ATTEMPTS {
        match transmitter.write(buffer) {
            Ok(transfer) => return Ok(transfer),
            Err((_, returned_transmitter, returned_buffer)) => {
                transmitter = returned_transmitter;
                buffer = returned_buffer;
                log::error!("I2S DMA restart attempt {attempt} of {MAX_RESTART_ATTEMPTS} failed");
                if attempt < MAX_RESTART_ATTEMPTS {
                    Timer::after(Duration::from_millis(1)).await;
                }
            }
        }
    }
    Err("transmit DMA restart failed repeatedly")
}

/// Mixes the next block into `mixed`, or silence while a writer holds the
/// mixer lock.
fn mix_block(
    system: Pin<&'static System>,
    output: &mut Output,
    mixed: &mut [u8; MIX_BYTES],
) -> core::result::Result<(), &'static str> {
    match system.mix_next_block() {
        Ok(()) => {
            let read = output.with_consumer(|consumer| consumer.read(mixed));
            if read != mixed.len() {
                return Err("the mixer did not produce a complete block");
            }
        }
        // Preserve queued PCM when a writer owns the mixer lock.
        Err(IoErrorKind::WouldBlock) => mixed.fill(0),
        Err(_) => return Err("the mixer reported an error"),
    }
    Ok(())
}

/// Encodes one mixed stereo F32 block as mono I16, sent in both TDM slots.
fn encode_block(destination: &mut [u8], mixed: &[u8; MIX_BYTES]) {
    let (frames, _) = destination.as_chunks_mut::<4>();
    for (input, frame) in mixed.as_chunks::<MIX_FRAME_BYTES>().0.iter().zip(frames) {
        let left = f32::from_le_bytes(*input.first_chunk().unwrap());
        let right = f32::from_le_bytes(*input.last_chunk().unwrap());
        let mono = ((left * 0.5 + right * 0.5) * 32768.0) as i16;
        let [low, high] = mono.to_le_bytes();
        *frame = [low, high, low, high];
    }
}

struct Playback {
    device: StreamDevice,
    state: &'static State,
}

impl Playback {
    fn check_output(&self) -> Result<()> {
        if self.state.failed.load(Ordering::Acquire) {
            return Err(IoErrorKind::DeviceUnavailable);
        }
        Ok(())
    }
}

impl BaseOperations for Playback {
    fn open(&self, context: &mut Context) -> Result<()> {
        self.check_output()?;
        self.device.open(context)
    }

    fn close(&self, context: &mut Context) -> Result<()> {
        self.device.close(context)
    }

    fn read(
        &self,
        context: &mut Context,
        task: &mut TaskContext<'_>,
        buffer: &mut [u8],
        position: Size,
    ) -> Poll<Result<usize>> {
        self.device.read(context, task, buffer, position)
    }

    fn write(
        &self,
        context: &mut Context,
        task: &mut TaskContext<'_>,
        buffer: &[u8],
        position: Size,
    ) -> Poll<Result<usize>> {
        self.check_output()?;
        self.device.write(context, task, buffer, position)
    }

    fn flush(&self, context: &mut Context, task: &mut TaskContext<'_>) -> Poll<Result<()>> {
        self.check_output()?;
        self.device.flush(context, task)
    }

    fn control(
        &self,
        context: &mut Context,
        task: &mut TaskContext<'_>,
        command: ControlCommandIdentifier,
        input: &AnyByLayout,
        output: &mut AnyByLayout,
    ) -> Poll<Result<()>> {
        // The underrun count stays readable after a fatal failure.
        if command == GET_UNDERRUNS::IDENTIFIER {
            return file_system::ready(|| {
                GET_UNDERRUNS::cast_input(input)?;
                *GET_UNDERRUNS::cast_output(output)? = self.state.underruns.load(Ordering::Relaxed);
                Ok(())
            });
        }
        self.check_output()?;
        self.device.control(context, task, command, input, output)
    }

    fn clone_context(&self, context: &Context) -> Result<Context> {
        self.device.clone_context(context)
    }
}

impl MountOperations for Playback {}
impl CharacterDevice for Playback {}
