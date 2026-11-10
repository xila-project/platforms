//! Native output for the fixed-capacity Xila audio service.

use ::audio::AudioSystem;
use ::audio::OutputReservation;
use nanoaudio_cpal::{Output, cpal::traits::HostTrait};
use std::{
    pin::Pin,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

const SAMPLE_RATE: u32 = 48_000;
#[cfg(test)]
const CHANNELS: u8 = 2;
const MAX_OUTPUTS: usize = 2;
/// Bytes of one stereo F32 frame in the fixed mix format.
const FRAME_BYTES: usize = 2 * size_of::<f32>();
/// Contention retries before a due block is postponed to the next wakeup.
const BUSY_RETRIES: u32 = 32;

/// Recommended native service: four 16 KiB (43 ms) application streams and
/// two 24 KiB (64 ms) output queues of 128-frame blocks.
///
/// Host audio servers request whole periods per callback (often 10 to 25 ms
/// through ALSA plugins), more than the core defaults queue. The mixer keeps
/// half of each output queue filled ahead of the device; Wasm guests refill
/// their stream from cooperative event loops whose turns exceed a few
/// milliseconds.
pub type System = AudioSystem<4, 16_384, 128, 2, 24_576>;

fn mix_period(frames: usize) -> Duration {
    let nanos = frames as u128 * 1_000_000_000 / SAMPLE_RATE as u128;
    Duration::from_nanos(nanos.min(u64::MAX as u128) as u64)
}

/// Whole mix blocks that fit in one output queue of `queue_bytes`.
fn queue_blocks(queue_bytes: usize, block_frames: usize) -> u32 {
    (queue_bytes / (block_frames * FRAME_BYTES).max(1)).min(u32::MAX as usize) as u32
}

/// Paces mixing on the host clock without losing blocks to late wakeups.
///
/// Every block that became due since the last call is rendered, so sleep
/// overshoot and lock contention do not slowly drain the output queues. When
/// the mixer falls behind by more than a whole queue, the outputs have already
/// underrun; the schedule restarts instead of rendering a burst nobody can hold.
struct Pacer {
    period: Duration,
    next: Instant,
    max_due: u32,
}

impl Pacer {
    fn new(now: Instant, period: Duration, max_due: u32) -> Self {
        Self {
            period,
            next: now + period,
            max_due: max_due.max(1),
        }
    }

    /// Returns the number of blocks due at `now` and advances the schedule.
    fn due(&mut self, now: Instant) -> u32 {
        if now < self.next || self.period.is_zero() {
            return 0;
        }
        let late = (now - self.next).as_nanos() / self.period.as_nanos();
        let due = late.saturating_add(1);
        if due > self.max_due as u128 {
            self.next = now + self.period;
            return self.max_due;
        }
        self.next += self.period * due as u32;
        due as u32
    }

    /// Time to sleep before the next block is due.
    fn wait(&self, now: Instant) -> Duration {
        self.next.saturating_duration_since(now)
    }
}

/// Mixes one block, retrying briefly while a writer holds the core lock.
///
/// Returns `Ok(false)` if the lock stayed busy; the block is retried at the
/// next wakeup rather than skipped.
fn mix_block<const S: usize, const B: usize, const F: usize, const O: usize, const Q: usize>(
    system: Pin<&AudioSystem<S, B, F, O, Q>>,
) -> Result<bool, file_system::Error> {
    for _ in 0..BUSY_RETRIES {
        match system.mix_next_block() {
            Ok(()) => return Ok(true),
            Err(file_system::Error::RessourceBusy) => thread::yield_now(),
            Err(error) => return Err(error),
        }
    }
    Ok(false)
}

fn output_configuration(
    configuration: ::audio::Configuration,
) -> Result<nanoaudio::Configuration, Error> {
    let format = match configuration.sample_format().map_err(Error::System)? {
        ::audio::SampleFormat::U8 => nanoaudio::Format::U8,
        ::audio::SampleFormat::I16 => nanoaudio::Format::I16,
        ::audio::SampleFormat::I24 => nanoaudio::Format::I24,
        ::audio::SampleFormat::I32 => nanoaudio::Format::I32,
        ::audio::SampleFormat::F32 => nanoaudio::Format::F32,
    };
    Ok(nanoaudio::Configuration::new(
        configuration.sample_rate,
        configuration.channels,
        format,
    ))
}

#[derive(Debug)]
pub enum Error {
    InvalidOutputCount,
    System(file_system::Error),
    Open(nanoaudio_cpal::Error),
    Play(nanoaudio_cpal::Error),
    MixFailed,
    MixThread(std::io::Error),
    OutputFailed,
    MixerStopped,
}

impl std::fmt::Display for Error {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InvalidOutputCount => write!(
                formatter,
                "native audio output count must be 1..={MAX_OUTPUTS}"
            ),
            Self::System(error) => {
                write!(formatter, "failed to reserve native audio output: {error}")
            }
            Self::Open(error) => write!(formatter, "failed to open native audio output: {error}"),
            Self::Play(error) => write!(formatter, "failed to start native audio output: {error}"),
            Self::MixFailed => write!(formatter, "native audio mixer failed"),
            Self::MixThread(error) => {
                write!(formatter, "failed to start native audio mixer: {error}")
            }
            Self::OutputFailed => write!(formatter, "native audio output reported a runtime error"),
            Self::MixerStopped => write!(formatter, "native audio mixer stopped unexpectedly"),
        }
    }
}

impl std::error::Error for Error {}

/// Owns the native output streams and the one scheduler that mixes for all sinks.
///
/// Output streams are opened during setup. The mixer thread only renders fixed
/// 128-frame quanta; cpal callbacks consume independent queues and insert
/// silence on underrun. Drop stops and joins the mixer before closing outputs.
pub struct Runner {
    outputs: Vec<NativeSink<Output, OutputReservation>>,
    stop: Arc<AtomicBool>,
    mixer_failed: Arc<AtomicBool>,
    mixer: Option<JoinHandle<()>>,
}

struct NativeSink<T, L> {
    output: Option<T>,
    reservation: Option<L>,
}

impl<T, L> NativeSink<T, L> {
    fn new(output: T, reservation: L) -> Self {
        Self {
            output: Some(output),
            reservation: Some(reservation),
        }
    }

    fn output(&self) -> &T {
        self.output.as_ref().expect("output exists until sink drop")
    }
}

impl<T, L> Drop for NativeSink<T, L> {
    fn drop(&mut self) {
        drop(self.output.take());
        drop(self.reservation.take());
    }
}

fn build_native_sink<const Q: usize, T, E>(
    consumer: nanoaudio::Consumer<'static, Q>,
    reservation: OutputReservation,
    build: impl FnOnce(nanoaudio::Consumer<'static, Q>) -> Result<T, E>,
) -> Result<NativeSink<T, OutputReservation>, E> {
    let output = build(consumer)?;
    Ok(NativeSink::new(output, reservation))
}

impl Runner {
    /// Returns output underruns accumulated by each native sink.
    pub fn underruns(&self) -> impl Iterator<Item = u32> + '_ {
        self.outputs.iter().map(|sink| sink.output().underruns())
    }

    /// Returns output callback errors accumulated by each native sink.
    pub fn output_errors(&self) -> impl Iterator<Item = u32> + '_ {
        self.outputs.iter().map(|sink| sink.output().errors())
    }

    /// Polls host output and mixer health without blocking.
    pub fn check(&self) -> Result<(), Error> {
        if self.mixer_failed.load(Ordering::Acquire) {
            return Err(Error::MixFailed);
        }
        if self.mixer.as_ref().is_some_and(JoinHandle::is_finished) {
            return Err(Error::MixerStopped);
        }
        if self.outputs.iter().any(|sink| sink.output().errors() != 0) {
            return Err(Error::OutputFailed);
        }
        Ok(())
    }
}

impl Drop for Runner {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(mixer) = self.mixer.take() {
            let _ = mixer.join();
        }
    }
}

/// Opens `output_count` host devices and starts one mixer for their shared mix.
///
/// The audio system must live in pinned static storage because cpal's callback
/// owns its output consumer for the lifetime of each stream. Only setup allocates;
/// the scheduling, mixing and cpal rendering paths use bounded queues. cpal
/// adapts the fixed 48 kHz stereo F32 mix to each selected device configuration.
pub fn new<const S: usize, const B: usize, const F: usize, const O: usize, const Q: usize>(
    system: Pin<&'static AudioSystem<S, B, F, O, Q>>,
    output_count: usize,
) -> Result<Runner, Error> {
    if !(1..=MAX_OUTPUTS).contains(&output_count) || output_count > O {
        return Err(Error::InvalidOutputCount);
    }
    let host = nanoaudio_cpal::cpal::default_host();
    // A single-output OS session follows the user's system default. Enumeration
    // order can otherwise choose an inactive HDMI output ahead of that device.
    let devices: Vec<_> = if output_count == 1 {
        vec![
            host.default_output_device()
                .ok_or(Error::Open(nanoaudio_cpal::Error::NoDevice))?,
        ]
    } else {
        host.output_devices()
            .map_err(|_| Error::Open(nanoaudio_cpal::Error::NoDevice))?
            .take(output_count)
            .collect()
    };
    if devices.len() != output_count {
        return Err(Error::Open(nanoaudio_cpal::Error::NoDevice));
    }
    let mut outputs = Vec::with_capacity(output_count);
    for device in devices {
        let sink = system.open_output().map_err(Error::System)?;
        let configuration = output_configuration(sink.configuration())?;
        let (consumer, reservation) = unsafe { sink.into_consumer_with_reservation() };
        let sink = build_native_sink(consumer, reservation, |consumer| {
            Output::open_device(
                &device,
                consumer,
                configuration,
                nanoaudio_cpal::Quality::default(),
            )
            .map_err(Error::Open)
        })?;
        sink.output().play().map_err(Error::Play)?;
        outputs.push(sink);
    }

    let stop = Arc::new(AtomicBool::new(false));
    let thread_stop = Arc::clone(&stop);
    let mixer_failed = Arc::new(AtomicBool::new(false));
    let thread_failed = Arc::clone(&mixer_failed);
    let mixer = thread::Builder::new()
        .name("xila-audio-mixer".into())
        .spawn(move || {
            let blocks = queue_blocks(Q, F);
            // Queue silence ahead of the device so that each host callback,
            // which can request several blocks at once, finds whole periods.
            let mut pending = (blocks / 2).max(1);
            let mut pacer = Pacer::new(Instant::now(), mix_period(F), blocks);
            while !thread_stop.load(Ordering::Acquire) {
                pending = pending
                    .saturating_add(pacer.due(Instant::now()))
                    .min(blocks);
                while pending > 0 {
                    match mix_block(system.as_ref()) {
                        Ok(true) => pending -= 1,
                        Ok(false) => break,
                        Err(_) => {
                            thread_failed.store(true, Ordering::Release);
                            return;
                        }
                    }
                }
                thread::sleep(pacer.wait(Instant::now()).max(Duration::from_micros(100)));
            }
        })
        .map_err(Error::MixThread)?;
    Ok(Runner {
        outputs,
        stop,
        mixer_failed,
        mixer: Some(mixer),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pin_static<T>(value: T) -> Pin<&'static T> {
        // SAFETY: the leaked allocation can never move or be freed.
        unsafe { Pin::new_unchecked(Box::leak(Box::new(value))) }
    }

    #[test]
    fn requests_fixed_system_mix_configuration() {
        let configuration = output_configuration(::audio::Configuration::new(
            SAMPLE_RATE,
            CHANNELS,
            ::audio::SampleFormat::F32,
        ))
        .unwrap();
        assert_eq!(configuration.sample_rate, 48_000);
        assert_eq!(configuration.channels, 2);
        assert_eq!(configuration.format, nanoaudio::Format::F32);
    }

    #[test]
    fn schedules_one_block_per_mix_period() {
        assert_eq!(mix_period(128).as_nanos(), 2_666_666);
        assert_eq!(mix_period(64).as_nanos(), 1_333_333);
        assert_eq!(mix_period(0), Duration::ZERO);
    }

    #[test]
    fn recommended_system_queues_whole_host_periods() {
        let system: Pin<&'static System> = pin_static(System::new().unwrap());
        assert_eq!(queue_blocks(24_576, 128), 24);
        // Half the output queue (32 ms) is kept ahead of the device.
        assert_eq!(mix_period(128 * 12).as_millis(), 32);
        // Each application stream holds 43 ms of PCM.
        let stream = system.open_stream().unwrap();
        assert_eq!(stream.writable(), 16_384);
        assert_eq!(mix_period(16_384 / FRAME_BYTES).as_millis(), 42);
    }

    #[test]
    fn pacer_renders_every_due_block_after_late_wakeups() {
        let start = Instant::now();
        let period = Duration::from_millis(2);
        let mut pacer = Pacer::new(start, period, 8);
        assert_eq!(pacer.due(start), 0);
        assert_eq!(pacer.wait(start), period);
        assert_eq!(pacer.due(start + period), 1);
        // A wakeup 3.5 periods late renders the three missed blocks.
        assert_eq!(pacer.due(start + period * 4 + period / 2), 3);
        assert_eq!(pacer.wait(start + period * 4 + period / 2), period / 2);
        // Ten periods total over 5 ms of sleep: no block is lost.
        assert_eq!(pacer.due(start + period * 10), 6);
    }

    #[test]
    fn pacer_restarts_after_falling_behind_a_whole_queue() {
        let start = Instant::now();
        let period = Duration::from_millis(2);
        let mut pacer = Pacer::new(start, period, 4);
        assert_eq!(pacer.due(start + period * 50), 4);
        assert_eq!(pacer.wait(start + period * 50), period);
        assert_eq!(pacer.due(start + period * 51), 1);
    }

    #[test]
    fn mix_block_renders_into_registered_outputs() {
        let system: Pin<&'static AudioSystem> = pin_static(AudioSystem::new().unwrap());
        let mut output = system.open_output().unwrap();
        let stream = system.open_stream().unwrap();
        stream.start().unwrap();
        assert_eq!(mix_block(system), Ok(true));
        let mut bytes = [0u8; 128 * FRAME_BYTES];
        let read = output.with_consumer(|consumer| consumer.read(&mut bytes));
        assert_eq!(read, bytes.len());
    }

    #[test]
    fn rejects_output_counts_outside_the_fixed_sink_limit() {
        assert_eq!(MAX_OUTPUTS, 2);
        assert!(!(1..=MAX_OUTPUTS).contains(&0));
        assert!(!(1..=MAX_OUTPUTS).contains(&(MAX_OUTPUTS + 1)));
    }

    #[test]
    fn invalid_output_count_fails_before_accessing_host_devices() {
        let system: Pin<&'static AudioSystem> = pin_static(AudioSystem::new().unwrap());
        assert!(matches!(new(system, 0), Err(Error::InvalidOutputCount)));
        assert!(matches!(new(system, 3), Err(Error::InvalidOutputCount)));
    }

    #[test]
    fn output_capacity_is_checked_before_host_device_access() {
        type OneOutputSystem = AudioSystem<4, 2048, 128, 1, 3072>;
        let system: Pin<&'static OneOutputSystem> = pin_static(OneOutputSystem::new().unwrap());
        assert!(matches!(new(system, 2), Err(Error::InvalidOutputCount)));
        assert!(system.open_output().is_ok());
    }

    #[test]
    fn native_sink_drops_backend_before_releasing_its_reservation() {
        use std::{cell::RefCell, rc::Rc};

        struct DropProbe(&'static str, Rc<RefCell<Vec<&'static str>>>);
        impl Drop for DropProbe {
            fn drop(&mut self) {
                self.1.borrow_mut().push(self.0);
            }
        }

        let dropped = Rc::new(RefCell::new(Vec::new()));
        drop(NativeSink::new(
            DropProbe("backend", Rc::clone(&dropped)),
            DropProbe("reservation", Rc::clone(&dropped)),
        ));
        assert_eq!(*dropped.borrow(), ["backend", "reservation"]);
    }

    #[test]
    fn dropping_native_sink_reclaims_its_system_slot() {
        let system: Pin<&'static AudioSystem> = pin_static(AudioSystem::new().unwrap());
        let (consumer, reservation) = unsafe {
            system
                .open_output()
                .unwrap()
                .into_consumer_with_reservation()
        };
        let sink = NativeSink::new(consumer, reservation);
        let second = system.open_output().unwrap();
        assert!(matches!(
            system.open_output(),
            Err(file_system::Error::RessourceBusy)
        ));
        drop(sink);
        let third = system.open_output().unwrap();
        drop((second, third));
    }

    #[test]
    fn failed_backend_open_releases_its_reservation_after_consumer_drop() {
        let system: Pin<&'static AudioSystem> = pin_static(AudioSystem::new().unwrap());
        let (consumer, reservation) = unsafe {
            system
                .open_output()
                .unwrap()
                .into_consumer_with_reservation()
        };
        let second = system.open_output().unwrap();
        let error = build_native_sink(consumer, reservation, |_consumer| {
            Err::<(), _>("adapter setup failed")
        });
        assert!(matches!(error, Err("adapter setup failed")));
        let third = system.open_output().unwrap();
        drop((second, third));
    }
}
