use core::{
    cell::RefCell,
    sync::atomic::{AtomicU8, Ordering},
    task::{Context, Poll},
};

use critical_section::Mutex;
use device::screen::{BRIGHTNESS_FULL, GET_BRIGHTNESS, SET_BRIGHTNESS};
use embedded_hal::pwm::SetDutyCycle;
use file_system::{
    ControlCommand, ControlCommandIdentifier, DirectBaseOperations, DirectCharacterDevice,
    IoErrorKind, MountOperations, Result, Size,
};
use shared::AnyByLayout;

/// A screen device whose backlight is dimmed by a PWM output.
///
/// Every operation goes to the wrapped screen, except the brightness
/// commands, which set the duty cycle of `backlight` in proportion to the
/// level: [`BRIGHTNESS_OFF`](device::screen::BRIGHTNESS_OFF) is a duty of
/// zero and [`BRIGHTNESS_FULL`] keeps the output always on.
pub struct BacklightScreenDevice<Backlight> {
    screen: &'static dyn DirectCharacterDevice,
    backlight: Mutex<RefCell<Backlight>>,
    level: AtomicU8,
}

impl<Backlight: SetDutyCycle> BacklightScreenDevice<Backlight> {
    /// Wraps `screen` and turns `backlight` fully on.
    pub fn new(screen: &'static dyn DirectCharacterDevice, backlight: Backlight) -> Result<Self> {
        let device = Self {
            screen,
            backlight: Mutex::new(RefCell::new(backlight)),
            level: AtomicU8::new(BRIGHTNESS_FULL),
        };
        device.set_brightness(BRIGHTNESS_FULL)?;
        Ok(device)
    }

    /// The current brightness level.
    pub fn brightness(&self) -> u8 {
        self.level.load(Ordering::Relaxed)
    }

    /// Sets the backlight duty cycle for `level`.
    pub fn set_brightness(&self, level: u8) -> Result<()> {
        critical_section::with(|section| {
            let mut backlight = self.backlight.borrow_ref_mut(section);
            backlight
                .set_duty_cycle_fraction(u16::from(level), u16::from(BRIGHTNESS_FULL))
                .map_err(|_| IoErrorKind::InputOutput)?;
            // Stored under the critical section so the level always matches
            // the last duty cycle written.
            self.level.store(level, Ordering::Relaxed);
            Ok(())
        })
    }
}

impl<Backlight: SetDutyCycle + Send> DirectBaseOperations for BacklightScreenDevice<Backlight> {
    fn read(
        &self,
        context: &mut Context<'_>,
        buffer: &mut [u8],
        position: Size,
    ) -> Poll<Result<usize>> {
        self.screen.read(context, buffer, position)
    }

    fn write(
        &self,
        context: &mut Context<'_>,
        buffer: &[u8],
        position: Size,
    ) -> Poll<Result<usize>> {
        self.screen.write(context, buffer, position)
    }

    fn flush(&self, context: &mut Context<'_>) -> Poll<Result<()>> {
        self.screen.flush(context)
    }

    fn control(
        &self,
        context: &mut Context<'_>,
        command: ControlCommandIdentifier,
        input: &AnyByLayout,
        output: &mut AnyByLayout,
    ) -> Poll<Result<()>> {
        match command {
            GET_BRIGHTNESS::IDENTIFIER => file_system::ready(|| {
                *GET_BRIGHTNESS::cast_output(output)? = self.brightness();
                Ok(())
            }),
            SET_BRIGHTNESS::IDENTIFIER => {
                file_system::ready(|| self.set_brightness(*SET_BRIGHTNESS::cast_input(input)?))
            }
            _ => self.screen.control(context, command, input, output),
        }
    }
}

impl<Backlight: SetDutyCycle + Send> MountOperations for BacklightScreenDevice<Backlight> {
    fn mount(&self) -> Result<()> {
        self.screen.mount()
    }

    fn unmount(&self) -> Result<()> {
        self.screen.unmount()
    }
}

impl<Backlight: SetDutyCycle + Send> DirectCharacterDevice for BacklightScreenDevice<Backlight> {
    fn readiness(
        &self,
        interests: file_system::ReadyEvents,
    ) -> Result<Option<file_system::ReadyEvents>> {
        self.screen.readiness(interests)
    }
}
