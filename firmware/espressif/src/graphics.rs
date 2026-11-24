//! The screen, its pointer and the graphics manager (feature `display`).

use alloc::{boxed::Box, vec::Vec};
use core::{
    cell::UnsafeCell,
    ptr::null_mut,
    slice,
    sync::atomic::{AtomicBool, Ordering},
    task::{Context, Poll},
};

use drivers_espressif::console;
use drivers_shared::devices::{
    display::{DEFAULT_REPORT_INTERVAL_MICROSECONDS, DisplayPerformance},
    touch::TouchSensor,
};
use esp_hal::time::Instant;
use xila::{
    file_system::{
        self, DirectBaseOperations, DirectCharacterDevice, IoErrorKind, MountOperations, Result,
        Size,
    },
    graphics::{self, Area, InputData, InputKind, Point, RenderingColor, State, lvgl},
    log, task, time,
};

use crate::{AsyncFlush, Display};

/// A cell that grants exclusive access to one caller at a time without masking
/// interrupts, so long bus transactions (such as I²C touch reads) do not stall
/// timers or serial interrupts.
struct Exclusive<T> {
    busy: AtomicBool,
    value: UnsafeCell<T>,
}

// SAFETY: `busy` serialises every mutable access to `value`.
unsafe impl<T: Send> Sync for Exclusive<T> {}

impl<T> Exclusive<T> {
    const fn new(value: T) -> Self {
        Self {
            busy: AtomicBool::new(false),
            value: UnsafeCell::new(value),
        }
    }

    fn try_with<R>(&self, function: impl FnOnce(&mut T) -> R) -> Option<R> {
        if self.busy.swap(true, Ordering::Acquire) {
            return None;
        }
        // SAFETY: `busy` was clear, so this caller holds exclusive access until
        // it is cleared again below.
        let result = function(unsafe { &mut *self.value.get() });
        self.busy.store(false, Ordering::Release);
        Some(result)
    }
}

/// Active asynchronous flush driver for the board's single display, initialized
/// once before installing the LVGL flush callbacks.
struct AsyncFlushSlot {
    initialized: AtomicBool,
    driver: UnsafeCell<Option<&'static dyn AsyncFlush>>,
}

// SAFETY: written once during `graphics::start` before setting `initialized`
// with `Release` ordering; read-only thereafter with `Acquire`.
unsafe impl Sync for AsyncFlushSlot {}

impl AsyncFlushSlot {
    const fn new() -> Self {
        Self {
            initialized: AtomicBool::new(false),
            driver: UnsafeCell::new(None),
        }
    }

    fn install(&self, driver: &'static dyn AsyncFlush) {
        assert!(
            !self.initialized.load(Ordering::Acquire),
            "asynchronous display flush driver was already installed"
        );
        // SAFETY: `initialized` is false and `graphics::start` runs once.
        unsafe {
            *self.driver.get() = Some(driver);
        }
        self.initialized.store(true, Ordering::Release);
    }

    fn get(&self) -> Option<&'static dyn AsyncFlush> {
        if !self.initialized.load(Ordering::Acquire) {
            return None;
        }
        // SAFETY: `driver` is immutable once `initialized` is true.
        unsafe { *self.driver.get() }
    }
}

static ASYNC_FLUSH: AsyncFlushSlot = AsyncFlushSlot::new();
static DISPLAY_PERFORMANCE: Exclusive<DisplayPerformance> = Exclusive::new(
    DisplayPerformance::new(DEFAULT_REPORT_INTERVAL_MICROSECONDS),
);

unsafe extern "C" fn async_flush_callback(
    display: *mut lvgl::lv_disp_t,
    area: *const lvgl::lv_area_t,
    data: *mut u8,
) {
    let Some(driver) = ASYNC_FLUSH.get() else {
        // SAFETY: LVGL calls this from its refresh loop with a valid display.
        unsafe { lvgl::lv_display_flush_ready(display) };
        return;
    };
    // SAFETY: LVGL passes a valid `lv_area_t` and a draw buffer containing at
    // least `width * height` pixels in the display's color format. Because the
    // graphics manager is leaked for `'static`, the draw buffers live for the
    // rest of the firmware's lifetime and LVGL calls `async_flush_wait_callback`
    // before reusing this buffer.
    let pending = unsafe {
        let lv_area = *area;
        let area = Area::new(
            Point::new(lv_area.x1 as i16, lv_area.y1 as i16),
            Point::new(lv_area.x2 as i16, lv_area.y2 as i16),
        );
        let byte_length = usize::from(area.get_width())
            * usize::from(area.get_height())
            * size_of::<RenderingColor>();
        let buffer = slice::from_raw_parts(data as *const u8, byte_length);
        driver.start_flush(area, buffer)
    };
    if !pending {
        // SAFETY: the transfer completed synchronously, so LVGL may reuse the
        // buffer immediately without invoking `flush_wait_cb`.
        unsafe { lvgl::lv_display_flush_ready(display) };
    }
}

unsafe extern "C" fn async_flush_wait_callback(_display: *mut lvgl::lv_disp_t) {
    if let Some(driver) = ASYNC_FLUSH.get() {
        driver.wait_for_flush();
    }
}

unsafe extern "C" fn round_area_even_width(event: *mut lvgl::lv_event_t) {
    // SAFETY: LVGL passes a mutable `*mut lv_area_t` as the event parameter for
    // `LV_EVENT_INVALIDATE_AREA`. On an even-width display, clearing bit 0 of
    // `x1` and setting bit 0 of `x2` keeps both coordinates inside `0..width`
    // while guaranteeing an even pixel count (4-byte-aligned RGB565 length).
    let area = unsafe { &mut *(lvgl::lv_event_get_param(event).cast::<lvgl::lv_area_t>()) };
    area.x1 &= !1;
    area.x2 |= 1;
}

unsafe extern "C" fn record_display_performance(event: *mut lvgl::lv_event_t) {
    if !console::maximum_level().enables(log::Level::Debug) {
        return;
    }
    let now = Instant::now().duration_since_epoch().as_micros();
    // SAFETY: LVGL passes a valid event handle to display event callbacks.
    let code = unsafe { lvgl::lv_event_get_code(event) };
    let report = DISPLAY_PERFORMANCE.try_with(|tracker| match code {
        lvgl::lv_event_code_t_LV_EVENT_RENDER_START => {
            tracker.on_render_start(now);
            None
        }
        lvgl::lv_event_code_t_LV_EVENT_FLUSH_START => {
            tracker.on_flush_start(now);
            None
        }
        lvgl::lv_event_code_t_LV_EVENT_FLUSH_FINISH => {
            tracker.on_flush_finish(now);
            None
        }
        lvgl::lv_event_code_t_LV_EVENT_FLUSH_WAIT_START => {
            tracker.on_wait_start(now);
            None
        }
        lvgl::lv_event_code_t_LV_EVENT_FLUSH_WAIT_FINISH => {
            tracker.on_wait_finish(now);
            None
        }
        lvgl::lv_event_code_t_LV_EVENT_RENDER_READY => tracker.on_render_ready(now),
        _ => None,
    });
    if let Some(Some(report)) = report {
        log::debug!("{report}");
    }
}

/// A pointer input device over a touch controller, polled by LVGL: each read
/// asks the controller for its current point.
pub struct TouchInputDevice<T: TouchSensor> {
    state: Exclusive<(T, Point)>,
}

impl<T: TouchSensor> TouchInputDevice<T> {
    pub const fn new(sensor: T) -> Self {
        Self {
            state: Exclusive::new((sensor, Point::new(0, 0))),
        }
    }
}

impl<T: TouchSensor + Send> DirectBaseOperations for TouchInputDevice<T> {
    fn read(&self, _: &mut Context<'_>, buffer: &mut [u8], _: Size) -> Poll<Result<usize>> {
        file_system::ready(|| {
            let data: &mut InputData = buffer
                .try_into()
                .map_err(|_| IoErrorKind::InvalidArgument)?;
            self.state
                .try_with(|(sensor, last)| {
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
                })
                .ok_or(IoErrorKind::ResourceBusy)?;
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

    let resolution = manager.get_resolution().await.unwrap_or_default();
    {
        let _lock = manager.lock().await;
        // SAFETY: the graphics lock is held, and the default display is the
        // one just created.
        unsafe {
            let lvgl_display = lvgl::lv_display_get_default();
            if display.byte_swapped_rgb565 {
                // Swapping the bytes keeps 2 bytes per pixel, so the draw
                // buffers stay valid.
                lvgl::lv_display_set_color_format(
                    lvgl_display,
                    lvgl::lv_color_format_t_LV_COLOR_FORMAT_RGB565_SWAPPED,
                );
            }
            if let Some(driver) = display.async_flush {
                ASYNC_FLUSH.install(driver);
                lvgl::lv_display_set_flush_cb(lvgl_display, Some(async_flush_callback));
                lvgl::lv_display_set_flush_wait_cb(lvgl_display, Some(async_flush_wait_callback));
            }
            if display.align_area_width_even && resolution.get_x() % 2 == 0 {
                lvgl::lv_display_add_event_cb(
                    lvgl_display,
                    Some(round_area_even_width),
                    lvgl::lv_event_code_t_LV_EVENT_INVALIDATE_AREA,
                    null_mut(),
                );
            }
            if let Some(period) = display.refresh_period_milliseconds {
                let refresh_timer = lvgl::lv_display_get_refr_timer(lvgl_display);
                if !refresh_timer.is_null() {
                    lvgl::lv_timer_set_period(refresh_timer, period);
                }
                let mut input_device = lvgl::lv_indev_get_next(null_mut());
                while !input_device.is_null() {
                    if lvgl::lv_indev_get_type(input_device)
                        == lvgl::lv_indev_type_t_LV_INDEV_TYPE_POINTER
                    {
                        let read_timer = lvgl::lv_indev_get_read_timer(input_device);
                        if !read_timer.is_null() {
                            lvgl::lv_timer_set_period(read_timer, period);
                        }
                    }
                    input_device = lvgl::lv_indev_get_next(input_device);
                }
            }
            for event_code in [
                lvgl::lv_event_code_t_LV_EVENT_RENDER_START,
                lvgl::lv_event_code_t_LV_EVENT_FLUSH_START,
                lvgl::lv_event_code_t_LV_EVENT_FLUSH_FINISH,
                lvgl::lv_event_code_t_LV_EVENT_FLUSH_WAIT_START,
                lvgl::lv_event_code_t_LV_EVENT_FLUSH_WAIT_FINISH,
                lvgl::lv_event_code_t_LV_EVENT_RENDER_READY,
            ] {
                lvgl::lv_display_add_event_cb(
                    lvgl_display,
                    Some(record_display_performance),
                    event_code,
                    null_mut(),
                );
            }
        }
    }

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

/// Shows a touch target on boards that have a display but no graphical shell.
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
