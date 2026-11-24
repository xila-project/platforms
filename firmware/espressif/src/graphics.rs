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

/// A pointer input device over a touch controller, polled by LVGL: each read
/// asks the controller for its current point.
pub struct TouchInputDevice<T: TouchSensor> {
    state: Mutex<RefCell<(T, Point)>>,
}

impl<T: TouchSensor> TouchInputDevice<T> {
    pub const fn new(sensor: T) -> Self {
        Self {
            state: Mutex::new(RefCell::new((sensor, Point::new(0, 0)))),
        }
    }
}

impl<T: TouchSensor + Send> DirectBaseOperations for TouchInputDevice<T> {
    fn read(&self, _: &mut Context<'_>, buffer: &mut [u8], _: Size) -> Poll<Result<usize>> {
        file_system::ready(|| {
            let data: &mut InputData = buffer
                .try_into()
                .map_err(|_| IoErrorKind::InvalidArgument)?;
            critical_section::with(|section| {
                let mut state = self.state.borrow(section).borrow_mut();
                let (sensor, last) = &mut *state;
                // A failed read counts as a release at the last point.
                let point = sensor.read().ok().flatten();
                if let Some((x, y)) = point {
                    *last = Point::new(x, y);
                }
                data.set(
                    *last,
                    if point.is_some() {
                        State::Pressed
                    } else {
                        State::Released
                    },
                );
            });
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

    // On ESP boards the graphics pointer is the built-in touch panel, and
    // the console input belongs to the separate serial session.
    if display.pointer.is_some() {
        manager.enable_on_screen_keyboard().await;
    }

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
