//! The screen, its pointer and the graphics manager (feature `graphics`).

use alloc::{boxed::Box, vec::Vec};
use core::{
    cell::RefCell,
    task::{Context, Poll},
};

use critical_section::Mutex;
use drivers_shared::devices::touch::TouchSensor;
use xila::{
    file_system::{
        self, DirectBaseOperations, DirectCharacterDevice, IoErrorKind, MountOperations, Result,
        Size,
    },
    graphics::{self, InputData, InputKind, Point, State, lvgl},
    log, task, time,
};

use crate::Display;

/// Taps logged by the touch diagnostics.
///
/// Temporary, for the WT32-SC01 Plus bring-up (#170): remove once touch input
/// is confirmed on the board. One warning line per tap keeps the console's
/// rate limit out of the way, since taps are seconds apart.
const DIAGNOSTIC_TAPS: u8 = 5;

/// What the diagnostics remember of the tap in progress.
#[derive(Clone, Copy)]
struct Tap {
    native: Option<(u16, u16)>,
    start: Point,
    samples: u16,
}

struct TouchState<T> {
    sensor: T,
    last: Point,
    tap: Option<Tap>,
    polled: bool,
    taps_logged: u8,
}

/// A diagnostic line to log after the state's critical section.
enum Diagnostic {
    FirstPoll,
    Tap { number: u8, tap: Tap, end: Point },
}

/// A pointer input device over a touch controller, polled by LVGL: each read
/// asks the controller for its current point.
pub struct TouchInputDevice<T: TouchSensor> {
    state: Mutex<RefCell<TouchState<T>>>,
}

impl<T: TouchSensor> TouchInputDevice<T> {
    pub const fn new(sensor: T) -> Self {
        Self {
            state: Mutex::new(RefCell::new(TouchState {
                sensor,
                last: Point::new(0, 0),
                tap: None,
                polled: false,
                taps_logged: 0,
            })),
        }
    }
}

impl<T: TouchSensor> TouchState<T> {
    /// Reads the controller and returns the point and state for LVGL, and a
    /// diagnostic to log.
    fn sample(&mut self) -> (Point, State, Option<Diagnostic>) {
        let mut diagnostic = (!self.polled).then_some(Diagnostic::FirstPoll);
        self.polled = true;
        // A failed read counts as a release at the last point.
        let point = self.sensor.read().ok().flatten();
        let Some((x, y)) = point else {
            if let Some(tap) = self.tap.take()
                && self.taps_logged < DIAGNOSTIC_TAPS
            {
                self.taps_logged += 1;
                diagnostic = Some(Diagnostic::Tap {
                    number: self.taps_logged,
                    tap,
                    end: self.last,
                });
            }
            return (self.last, State::Released, diagnostic);
        };
        self.last = Point::new(x, y);
        match &mut self.tap {
            Some(tap) => tap.samples = tap.samples.saturating_add(1),
            None => {
                self.tap = Some(Tap {
                    native: self.sensor.last_native_point(),
                    start: self.last,
                    samples: 1,
                })
            }
        }
        (self.last, State::Pressed, diagnostic)
    }
}

fn log_diagnostic(diagnostic: Diagnostic) {
    match diagnostic {
        Diagnostic::FirstPoll => {
            log::warning!("Xila: touch diagnostics: LVGL polls the touch panel")
        }
        Diagnostic::Tap { number, tap, end } => {
            let (native_x, native_y) = tap.native.unwrap_or_default();
            log::warning!(
                "Xila: touch diagnostics: tap {number}: pressed at panel ({native_x}, {native_y}), \
                 screen ({}, {}); released at screen ({}, {}) after {} samples",
                tap.start.get_x(),
                tap.start.get_y(),
                end.get_x(),
                end.get_y(),
                tap.samples
            );
        }
    }
}

impl<T: TouchSensor + Send> DirectBaseOperations for TouchInputDevice<T> {
    fn read(&self, _: &mut Context<'_>, buffer: &mut [u8], _: Size) -> Poll<Result<usize>> {
        file_system::ready(|| {
            let data: &mut InputData = buffer
                .try_into()
                .map_err(|_| IoErrorKind::InvalidArgument)?;
            let (point, state, diagnostic) =
                critical_section::with(|section| self.state.borrow(section).borrow_mut().sample());
            // These are exactly the point and state that LVGL receives.
            data.set(point, state);
            if let Some(diagnostic) = diagnostic {
                log_diagnostic(diagnostic);
            }
            Ok(size_of::<InputData>())
        })
    }

    fn write(&self, _: &mut Context<'_>, _: &[u8], _: Size) -> Poll<Result<usize>> {
        Poll::Ready(Err(IoErrorKind::Unsupported))
    }
}

impl<T: TouchSensor + Send> MountOperations for TouchInputDevice<T> {}
impl<T: TouchSensor + Send> DirectCharacterDevice for TouchInputDevice<T> {}

/// Starts the graphics manager on the board's screen and spawns its loop.
pub(crate) async fn start(
    task_manager: &'static task::Manager,
    task: task::TaskIdentifier,
    time_manager: &'static time::Manager<'static>,
    display: Display,
) -> &'static graphics::Manager {
    let inputs: Vec<(&'static dyn DirectCharacterDevice, InputKind)> = display
        .pointer
        .map(|pointer| (pointer, InputKind::Pointer))
        .into_iter()
        .collect();

    let heap = crate::heap();
    // The draw buffers are allocated now; keep them where DMA reads them
    // fastest when the board asks for it.
    heap.set_internal_only(display.internal_draw_buffers);
    let manager = graphics::initialize_with_input_devices(
        time_manager,
        display.screen,
        &inputs,
        display.draw_buffer_pixels,
        display.double_buffered,
    )
    .await;
    heap.set_internal_only(false);

    let manager: &'static graphics::Manager = Box::leak(Box::new(manager));
    graphics::set_ffi_manager(manager);

    if display.byte_swapped_rgb565 {
        let _lock = manager.lock().await;
        // SAFETY: the graphics lock is held, and the default display is the
        // one just created. Swapping the bytes keeps 2 bytes per pixel, so
        // the draw buffers stay valid.
        unsafe {
            lvgl::lv_display_set_color_format(
                lvgl::lv_display_get_default(),
                lvgl::lv_color_format_t_LV_COLOR_FORMAT_RGB565_SWAPPED,
            );
        }
    }

    let resolution = manager.get_resolution().await.unwrap_or_default();
    log::information!(
        "Xila: graphics on a {}x{} screen, {}-pixel draw buffer{}",
        resolution.get_x(),
        resolution.get_y(),
        display.draw_buffer_pixels,
        if display.double_buffered { "s" } else { "" }
    );

    task_manager
        .spawn(task, "Graphics", None, |_| {
            manager.r#loop(task::Manager::sleep)
        })
        .await
        .expect("Failed to spawn the graphics loop");
    manager
}
