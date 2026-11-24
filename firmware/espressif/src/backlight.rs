//! Screen backlights dimmed by the LED controller (LEDC).
//!
//! The backlight pin carries a 25 kHz PWM signal: above the audible range, so
//! the backlight driver does not whine, and far above any visible flicker. The
//! 10-bit duty resolution gives 1024 steps, more than the 256 brightness
//! levels. [`attach`] wraps the board's screen in a
//! [`BacklightScreenDevice`], which turns the brightness commands of Xila's
//! screen contract into duty cycles; the level starts at full brightness.

use alloc::{boxed::Box, format, string::String};

use drivers_shared::devices::display::BacklightScreenDevice;
use embedded_hal::pwm::{ErrorType, SetDutyCycle};
use esp_hal::{
    gpio::{DriveMode, interconnect::PeripheralOutput},
    ledc::{
        LSGlobalClkSource, Ledc, LowSpeed,
        channel::{self, Channel, ChannelIFace},
        timer::{self, TimerIFace},
    },
    peripherals::LEDC,
    time::Rate,
};

use crate::Display;

/// The PWM frequency on the backlight pin.
const FREQUENCY_KILOHERTZ: u32 = 25;

/// A backlight output on LEDC low-speed timer 0 and channel 0.
pub struct Backlight(Channel<'static, LowSpeed>);

// SAFETY: the channel holds a shared reference to its leaked, never modified
// timer and to the LEDC registers. It is not `Send` only because those types
// are not `Sync`; `BacklightScreenDevice` moves it once and then uses it only
// inside a critical section, so no two contexts touch the registers at once.
unsafe impl Send for Backlight {}

impl ErrorType for Backlight {
    type Error = channel::Error;
}

impl SetDutyCycle for Backlight {
    fn max_duty_cycle(&self) -> u16 {
        self.0.max_duty_cycle()
    }

    fn set_duty_cycle(&mut self, duty: u16) -> Result<(), Self::Error> {
        self.0.set_duty_cycle(duty)
    }
}

/// Starts PWM on `pin` with the backlight off, so that the panel can be
/// cleared before it is lit.
pub fn start(
    ledc: LEDC<'static>,
    pin: impl PeripheralOutput<'static>,
) -> Result<Backlight, String> {
    let mut ledc = Ledc::new(ledc);
    ledc.set_global_slow_clock(LSGlobalClkSource::APBClk);
    // The channel borrows its timer for as long as it runs: for ever.
    let timer = Box::leak(Box::new(ledc.timer::<LowSpeed>(timer::Number::Timer0)));
    timer
        .configure(timer::config::Config {
            duty: timer::config::Duty::Duty10Bit,
            clock_source: timer::LSClockSource::APBClk,
            frequency: Rate::from_khz(FREQUENCY_KILOHERTZ),
        })
        .map_err(|error| format!("cannot configure the backlight timer: {error:?}"))?;
    let mut channel = ledc.channel(channel::Number::Channel0, pin);
    channel
        .configure(channel::config::Config {
            timer: timer as &'static dyn TimerIFace<LowSpeed>,
            duty_pct: 0,
            drive_mode: DriveMode::PushPull,
        })
        .map_err(|error| format!("cannot configure the backlight channel: {error:?}"))?;
    Ok(Backlight(channel))
}

/// Gives `display` brightness control through `backlight`, and turns the
/// backlight fully on.
pub fn attach(display: &mut Display, backlight: Backlight) -> Result<(), String> {
    let screen = BacklightScreenDevice::new(display.screen, backlight)
        .map_err(|error| format!("cannot turn the backlight on: {error:?}"))?;
    display.screen = Box::leak(Box::new(screen));
    Ok(())
}
