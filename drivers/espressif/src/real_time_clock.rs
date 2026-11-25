//! Internal real-time clock of the ESP32 family, over esp-hal's `rtc_cntl::Rtc`.
//!
//! esp-hal keeps "boot time" in the always-on `LP_AON` store registers, so
//! `Rtc::current_time_us` survives software resets, watchdog resets and deep
//! sleep, but **not power-off**: the registers are lost when the chip loses
//! power, because no ESP32 has a battery-backed domain. esp-hal does not clear
//! the store registers either, so after a power-on the reading can be garbage;
//! the ceiling below rejects it. The clock therefore keeps wall-clock time
//! (UTC) across resets only.
//!
//! Accuracy depends on the low-power clock source: with the internal RC
//! oscillator it drifts, and the error grows with the time elapsed since the
//! clock was last set while the chip stayed powered, so this clock is a
//! wall-clock fallback, not a precise time source. Xila never derives timers
//! or deadlines from it; only UTC is seeded from it at start-up and written
//! back to it.

use core::time::Duration;

use esp_hal::rtc_cntl::Rtc;
use peripherals::real_time_clock::{
    CENTURY_SECONDS, Error, RealTimeClockController, Time, microseconds_from_unix_time,
    unix_seconds_in_range, unix_time_from_microseconds_in_range,
};

/// The ESP32's internal real-time clock.
///
/// `minimum_unix_seconds` is the build date of the firmware: a reading earlier
/// than it means the clock was never set or lost power, and [`read`] reports no
/// valid time instead. Readings later than the build date plus
/// [`CENTURY_SECONDS`] are rejected the same way, so garbage in the store
/// registers after a power-on is never accepted. [`write`] rejects times
/// outside the same range with [`Error::Invalid`], so a write can never store a
/// time that the next boot would ignore.
///
/// [`read`]: RealTimeClockController::read
/// [`write`]: RealTimeClockController::write
pub struct InternalRealTimeClock {
    rtc: Rtc<'static>,
    minimum_unix_seconds: i64,
    maximum_unix_seconds: i64,
}

impl InternalRealTimeClock {
    /// Wraps `rtc` (from `Rtc::new(peripherals.RTC_TIMER)`) with the platform's
    /// minimum timestamp, the build date in Unix seconds.
    pub fn new(rtc: Rtc<'static>, minimum_unix_seconds: i64) -> Self {
        Self {
            rtc,
            minimum_unix_seconds,
            maximum_unix_seconds: minimum_unix_seconds + CENTURY_SECONDS,
        }
    }
}

impl RealTimeClockController for InternalRealTimeClock {
    fn read(&self) -> Result<Option<Time>, Error> {
        Ok(unix_time_from_microseconds_in_range(
            self.rtc.current_time_us(),
            self.minimum_unix_seconds,
            self.maximum_unix_seconds,
        ))
    }

    fn write(&self, time: Time) -> Result<(), Error> {
        if !unix_seconds_in_range(
            time.seconds,
            self.minimum_unix_seconds,
            self.maximum_unix_seconds,
        ) {
            return Err(Error::Invalid);
        }
        let microseconds = microseconds_from_unix_time(time).ok_or(Error::Invalid)?;
        self.rtc.set_current_time_us(microseconds);
        Ok(())
    }

    fn resolution(&self) -> Duration {
        Duration::from_micros(1)
    }
}
