use alloc::{boxed::Box, string::String};
use drivers_shared::devices::SharedPointerQueue;
use file_system::{DirectBaseOperations, DirectCharacterDevice, MountOperations, Size};
use graphics::{InputData, Point, State};
use synchronization::Arc;
use wasm_bindgen::{JsCast, prelude::Closure};
use web_sys::{HtmlCanvasElement, MouseEvent, TouchEvent};

/// Pointer events queue up between LVGL reads, so a press and its release that arrive within one
/// frame both reach LVGL.
pub struct MouseDevice(Arc<SharedPointerQueue>);

fn clamp_coordinate(value: i32) -> i16 {
    value.clamp(i16::MIN as i32, i16::MAX as i32) as i16
}

impl MouseDevice {
    pub fn new(canvas: &HtmlCanvasElement) -> Result<Self, String> {
        let queue = Arc::new(SharedPointerQueue::new());

        let queue_clone = queue.clone();
        let closure = Closure::wrap(Box::new(move |event: MouseEvent| {
            if queue_clone
                .update(
                    clamp_coordinate(event.client_x()),
                    clamp_coordinate(event.client_y()),
                    event.buttons() != 0,
                )
                .is_err()
            {
                log::warning!("Pointer queue full, dropping {} event", event.type_());
            }
        }) as Box<dyn FnMut(MouseEvent)>);

        canvas
            .add_event_listener_with_callback("mousedown", closure.as_ref().unchecked_ref())
            .map_err(|_| "Failed to add mousedown event listener")?;
        canvas
            .add_event_listener_with_callback("mouseup", closure.as_ref().unchecked_ref())
            .map_err(|_| "Failed to add mouseup event listener")?;
        canvas
            .add_event_listener_with_callback("mousemove", closure.as_ref().unchecked_ref())
            .map_err(|_| "Failed to add mousemove event listener")?;
        canvas
            .add_event_listener_with_callback("mouseleave", closure.as_ref().unchecked_ref())
            .map_err(|_| "Failed to add mouseleave event listener")?;
        closure.forget(); // Prevent memory leak by keeping the closure alive

        // Add touch event support
        let queue_clone = queue.clone();
        let touch_closure = Closure::wrap(Box::new(move |event: TouchEvent| {
            event.prevent_default(); // Prevent default touch behavior

            // Get the first touch point
            let result = if let Some(touch) = event.touches().get(0) {
                // Determine state based on event type
                queue_clone.update(
                    clamp_coordinate(touch.client_x()),
                    clamp_coordinate(touch.client_y()),
                    matches!(event.type_().as_str(), "touchstart" | "touchmove"),
                )
            } else {
                // No touches remaining (touchend, touchcancel): release where the finger was.
                queue_clone.set_pressed(false)
            };
            if result.is_err() {
                log::warning!("Pointer queue full, dropping {} event", event.type_());
            }
        }) as Box<dyn FnMut(TouchEvent)>);

        canvas
            .add_event_listener_with_callback("touchstart", touch_closure.as_ref().unchecked_ref())
            .map_err(|_| "Failed to add touchstart event listener")?;
        canvas
            .add_event_listener_with_callback("touchend", touch_closure.as_ref().unchecked_ref())
            .map_err(|_| "Failed to add touchend event listener")?;
        canvas
            .add_event_listener_with_callback("touchmove", touch_closure.as_ref().unchecked_ref())
            .map_err(|_| "Failed to add touchmove event listener")?;
        canvas
            .add_event_listener_with_callback("touchcancel", touch_closure.as_ref().unchecked_ref())
            .map_err(|_| "Failed to add touchcancel event listener")?;
        touch_closure.forget(); // Prevent memory leak by keeping the closure alive

        Ok(Self(queue))
    }
}

impl DirectBaseOperations for MouseDevice {
    fn read(
        &self,
        _cx: &mut core::task::Context<'_>,
        buffer: &mut [u8],
        _: Size,
    ) -> core::task::Poll<file_system::Result<usize>> {
        file_system::ready(|| {
            let data: &mut InputData = buffer
                .try_into()
                .map_err(|_| file_system::IoErrorKind::InvalidArgument)?;

            // One queued sample per read; LVGL reads again while `continue` is set.
            let (sample, more) = self.0.next_sample();
            data.set(
                Point::new(sample.x, sample.y),
                if sample.pressed {
                    State::Pressed
                } else {
                    State::Released
                },
            );
            data.set_continue(more);

            Ok(buffer.len())
        })
    }

    fn write(
        &self,
        _cx: &mut core::task::Context<'_>,
        _: &[u8],
        _: Size,
    ) -> core::task::Poll<file_system::Result<usize>> {
        file_system::ready(|| Err(file_system::IoErrorKind::Unsupported))
    }
}

impl MountOperations for MouseDevice {}

impl DirectCharacterDevice for MouseDevice {}
