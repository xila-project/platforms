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
        "Xila: graphics on a {}x{} screen, {}-pixel RGB565 draw buffer{} ({} bytes each)",
        resolution.get_x(),
        resolution.get_y(),
        display.draw_buffer_pixels,
        if display.double_buffered { "s" } else { "" },
        display.draw_buffer_pixels * 2,
    );

    task_manager
        .spawn(task, "Graphics", None, |_| {
            manager.r#loop(task::Manager::sleep)
        })
        .await
        .expect("Failed to spawn the graphics loop");
    manager
}

/// Gives the classic board a touch target while measuring its boot heap.
#[cfg(not(feature = "graphics"))]
pub(crate) async fn show_touch_test(manager: &graphics::Manager) {
    unsafe extern "C" fn show_touch(event: *mut lvgl::lv_event_t) {
        // SAFETY: LVGL calls this while holding the graphics lock. The label
        // belongs to the button that owns the event callback.
        unsafe {
            let label = lvgl::lv_event_get_user_data(event).cast();
            lvgl::lv_label_set_text_static(label, c"Touch detected".as_ptr());
        }
    }

    let _lock = manager.lock().await;
    // SAFETY: the graphics lock is held and a display has been initialized.
    unsafe {
        let button = lvgl::lv_button_create(lvgl::lv_screen_active());
        if button.is_null() {
            return;
        }
        lvgl::lv_obj_set_size(button, 200, 80);
        lvgl::lv_obj_set_pos(button, 40, 40);
        let label = lvgl::lv_label_create(button);
        if label.is_null() {
            lvgl::lv_obj_delete(button);
            return;
        }
        lvgl::lv_label_set_text_static(label, c"Touch test".as_ptr());
        lvgl::lv_obj_center(label);
        lvgl::lv_obj_add_event_cb(
            button,
            Some(show_touch),
            lvgl::lv_event_code_t_LV_EVENT_CLICKED,
            label.cast(),
        );
    }
}
