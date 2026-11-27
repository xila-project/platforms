//! PCM playback through I2S transmit DMA on the ESP32-S3.

use alloc::boxed::Box;
use core::{
    pin::Pin,
    sync::atomic::{AtomicBool, Ordering},
    task::{Context as TaskContext, Poll},
};

use audio::{AudioStreamDevice, AudioSystem, OutputHandle};
use embassy_time::{Duration, with_timeout};
use esp_hal::{
    Async,
    dma::{DmaDescriptor, DmaTxStreamBuf},
    i2s::master::{ConfigError, I2sTx, TdmConfig},
    time::Rate,
};
use file_system::{
    BaseOperations, CharacterDevice, Context, ControlCommandIdentifier, IntoIoErrorKind,
    IoErrorKind, MountOperations, Result, Size,
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

type System = AudioSystem<STREAMS, STREAM_BYTES, FRAMES, 1, MIX_BYTES>;
type Output = OutputHandle<'static, STREAMS, STREAM_BYTES, FRAMES, 1, MIX_BYTES>;
type StreamDevice = AudioStreamDevice<STREAMS, STREAM_BYTES, FRAMES, 1, MIX_BYTES>;

/// The board transmitter or the error returned when configuring it.
pub type Transmitter = core::result::Result<I2sTx<'static, Async>, ConfigError>;

/// The on-wire format for the amplifier.
pub fn configuration() -> TdmConfig {
    TdmConfig::new_tdm_philips().with_sample_rate(Rate::from_hz(SAMPLE_RATE))
}

/// Mounts the PCM device and starts its DMA task. The device reports an I/O
/// error after a transfer failure, including to clients with an open stream.
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
    static FAILED: AtomicBool = AtomicBool::new(false);
    file_system
        .mount_character_device(
            task,
            PLAYBACK_PATH,
            Playback {
                device: StreamDevice::new(system),
                failed: &FAILED,
            },
        )
        .await?;

    let transmitter = transmitter.map_err(|error| {
        FAILED.store(true, Ordering::Release);
        log::error!("I2S configuration failed: {error:?}");
        IoErrorKind::DeviceUnavailable
    })?;
    tasks
        .spawn(task, "Speaker output", None, async move |_| {
            let result = transmit(transmitter, system, output).await;
            FAILED.store(true, Ordering::Release);
            if let Err(error) = result {
                log::error!("I2S playback failed: {error}");
            }
        })
        .await
        .map_err(|error| {
            FAILED.store(true, Ordering::Release);
            error.into_io_error_kind()
        })?;
    Ok(())
}

async fn transmit(
    transmitter: I2sTx<'static, Async>,
    system: Pin<&'static System>,
    mut output: Output,
) -> core::result::Result<(), &'static str> {
    // The static DMA buffer is in internal memory. Eight descriptors provide
    // 42.7 ms of audio while other tasks run. Start with silence in each one.
    let buffer: DmaTxStreamBuf = esp_hal::dma_tx_stream_buffer!(DMA_BUFFER_BYTES, TRANSMIT_BYTES);
    let mut transfer = transmitter
        .write(buffer)
        .map_err(|_| "could not start transmit DMA")?;
    let mut mixed = [0; MIX_BYTES];

    loop {
        with_timeout(
            Duration::from_millis(100),
            transfer.wait_for_available_async(),
        )
        .await
        .map_err(|_| "transmit DMA timed out")?
        .map_err(|_| "transmit DMA stopped or reported a descriptor error")?;

        let available_blocks = transfer.available_bytes() / TRANSMIT_BYTES;
        for _ in 0..available_blocks {
            match system.mix_next_block() {
                Ok(()) => {
                    let read = output.with_consumer(|consumer| consumer.read(&mut mixed));
                    if read != mixed.len() {
                        return Err("the mixer did not produce a complete block");
                    }
                }
                // Preserve queued PCM when a writer owns the mixer lock.
                Err(IoErrorKind::WouldBlock) => mixed.fill(0),
                Err(_) => return Err("the mixer reported an error"),
            }
            transfer.push_with(|buffer| {
                let (frames, _) = buffer[..TRANSMIT_BYTES].as_chunks_mut::<4>();
                for (input, frame) in mixed.as_chunks::<MIX_FRAME_BYTES>().0.iter().zip(frames) {
                    let left = f32::from_le_bytes(*input.first_chunk().unwrap());
                    let right = f32::from_le_bytes(*input.last_chunk().unwrap());
                    let mono = ((left * 0.5 + right * 0.5) * 32768.0) as i16;
                    let [low, high] = mono.to_le_bytes();
                    *frame = [low, high, low, high];
                }
                TRANSMIT_BYTES
            });
        }
    }
}

struct Playback {
    device: StreamDevice,
    failed: &'static AtomicBool,
}

impl Playback {
    fn check_output(&self) -> Result<()> {
        if self.failed.load(Ordering::Acquire) {
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
        self.check_output()?;
        self.device.control(context, task, command, input, output)
    }

    fn clone_context(&self, context: &Context) -> Result<Context> {
        self.device.clone_context(context)
    }
}

impl MountOperations for Playback {}
impl CharacterDevice for Playback {}
