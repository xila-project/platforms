mod counter;

use core::{
    cell::RefCell,
    task::{Context, Poll, Waker},
};

use critical_section::Mutex;
use embassy_time_driver::Driver;
use embassy_time_queue_utils::Queue;
use esp_hal::{Blocking, time::Duration, timer::OneShotTimer};
use file_system::{
    DirectBaseOperations, DirectCharacterDevice, IoErrorKind, MountOperations, Result, Size,
};

use counter::{Counter, alarm_has_elapsed};

#[cfg(not(feature = "esp32"))]
use esp_hal::{
    peripherals::SYSTIMER,
    timer::systimer::{SystemTimer, Unit},
};
#[cfg(feature = "esp32")]
use esp_hal::{time::Instant, timer::timg::Timer};

// Split long Embassy deadlines into delays that fit either hardware timer.
const MAXIMUM_ALARM_MICROSECONDS: u64 = 60_000_000;
const _: () = assert!(embassy_time_driver::TICK_HZ == 1_000_000);

struct Alarm {
    timer: OneShotTimer<'static, Blocking>,
    counter: Counter,
}

impl Alarm {
    fn now(&mut self) -> u64 {
        self.counter.microseconds(counter_value())
    }

    fn schedule(&mut self, queue: &mut Queue) {
        loop {
            let now = self.now();
            let expiration = queue.next_expiration(now);
            if expiration == u64::MAX {
                self.timer.unlisten();
                self.timer.stop();
                self.timer.clear_interrupt();
                return;
            }

            let delay = (expiration - now).min(MAXIMUM_ALARM_MICROSECONDS);
            self.timer
                .schedule(Duration::from_micros(delay))
                .expect("alarm delay must fit the hardware timer");
            self.timer.listen();

            // The system timer compares for equality. A short delay can pass
            // while programming the comparator. Sample the counter again and
            // wake expired tasks in software before arming the next deadline.
            if !alarm_has_elapsed(now, delay, self.now()) {
                return;
            }
        }
    }
}

struct TimerState {
    queue: Queue,
    alarm: Option<Alarm>,
}

struct TimeDriver(Mutex<RefCell<TimerState>>);

impl TimeDriver {
    const fn new() -> Self {
        Self(Mutex::new(RefCell::new(TimerState {
            queue: Queue::new(),
            alarm: None,
        })))
    }

    fn with_state<T>(&self, function: impl FnOnce(&mut TimerState) -> T) -> T {
        critical_section::with(|section| function(&mut self.0.borrow(section).borrow_mut()))
    }
}

impl Driver for TimeDriver {
    fn now(&self) -> u64 {
        self.with_state(|state| state.alarm.as_mut().map_or(0, Alarm::now))
    }

    fn schedule_wake(&self, at: u64, waker: &Waker) {
        self.with_state(|state| {
            if state.queue.schedule_wake(at, waker)
                && let Some(alarm) = &mut state.alarm
            {
                alarm.schedule(&mut state.queue);
            }
        });
    }
}

embassy_time_driver::time_driver_impl!(static DRIVER: TimeDriver = TimeDriver::new());

#[esp_hal::handler]
fn alarm_handler() {
    DRIVER.with_state(|state| {
        if let Some(alarm) = &mut state.alarm {
            alarm.timer.clear_interrupt();
            alarm.schedule(&mut state.queue);
        }
    });
}

#[cfg(feature = "esp32")]
fn counter_value() -> u64 {
    Instant::now().duration_since_epoch().as_micros()
}

#[cfg(not(feature = "esp32"))]
fn counter_value() -> u64 {
    SystemTimer::unit_value(Unit::Unit0)
}

/// Initializes the one-megahertz Embassy clock using a timer group alarm.
///
/// Call after initializing esp-hal and before starting the executor.
#[cfg(feature = "esp32")]
pub fn initialize(timer: Timer<'static>) {
    // The HAL converts the ESP32's 64-bit, 16-megahertz counter to microseconds.
    initialize_alarm(OneShotTimer::new(timer), u64::MAX >> 4, 1_000_000);
}

/// Initializes the one-megahertz Embassy clock using system timer alarm zero.
///
/// Call after initializing esp-hal and before starting the executor.
#[cfg(not(feature = "esp32"))]
pub fn initialize(system_timer: SYSTIMER<'static>) {
    initialize_alarm(
        OneShotTimer::new(SystemTimer::new(system_timer).alarm0),
        SystemTimer::BIT_MASK,
        SystemTimer::ticks_per_second(),
    );
}

fn initialize_alarm(mut timer: OneShotTimer<'static, Blocking>, mask: u64, frequency: u64) {
    DRIVER.with_state(|state| {
        assert!(
            state.alarm.is_none(),
            "the Embassy clock is already initialized"
        );
        timer.set_interrupt_handler(alarm_handler);
        let mut alarm = Alarm {
            timer,
            counter: Counter::new(counter_value(), mask, frequency),
        };
        alarm.schedule(&mut state.queue);
        state.alarm = Some(alarm);
    });
}

/// Hardware time device for boards without a configured wall clock.
#[derive(Clone, Copy, Debug, Default)]
pub struct TimeDevice;

impl DirectBaseOperations for TimeDevice {
    fn read(&self, _: &mut Context<'_>, _: &mut [u8], _: Size) -> Poll<Result<usize>> {
        Poll::Ready(Err(IoErrorKind::DeviceUnavailable))
    }

    fn write(&self, _: &mut Context<'_>, _: &[u8], _: Size) -> Poll<Result<usize>> {
        Poll::Ready(Err(IoErrorKind::Unsupported))
    }
}

impl MountOperations for TimeDevice {}
impl DirectCharacterDevice for TimeDevice {}

impl time::RealtimeSource for TimeDevice {
    fn read(&self) -> time::Result<time::RealtimeSample> {
        Err(time::Error::Unsynchronized)
    }
}
