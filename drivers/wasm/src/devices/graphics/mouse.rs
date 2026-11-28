use alloc::{boxed::Box, format, string::String};
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
            queue_clone.update(
                clamp_coordinate(event.client_x()),
                clamp_coordinate(event.client_y()),
                event.buttons() != 0,
            );
        }) as Box<dyn FnMut(MouseEvent)>);

        for event in ["mousedown", "mousemove", "mouseleave"] {
            canvas
                .add_event_listener_with_callback(event, closure.as_ref().unchecked_ref())
                .map_err(|_| format!("Failed to add {event} event listener"))?;
        }
        // The release is listened for on the window: a button released after the pointer left the
        // canvas is never delivered to the canvas and would leave the press stuck.
        web_sys::window()
            .ok_or("No window available")?
            .add_event_listener_with_callback("mouseup", closure.as_ref().unchecked_ref())
            .map_err(|_| "Failed to add mouseup event listener")?;
        closure.forget(); // Prevent memory leak by keeping the closure alive

        // Add touch event support. Only the first finger drives the pointer: other fingers are
        // ignored, and lifting one of them must not release the pointer.
        let queue_clone = queue.clone();
        let mut primary_touch: Option<i32> = None;
        let touch_closure = Closure::wrap(Box::new(move |event: TouchEvent| {
            event.prevent_default(); // Prevent default touch behavior

            let find = |list: web_sys::TouchList, identifier: i32| {
                (0..list.length())
                    .filter_map(|index| list.get(index))
                    .find(|touch| touch.identifier() == identifier)
            };
            let position = |touch: &web_sys::Touch| {
                (
                    clamp_coordinate(touch.client_x()),
                    clamp_coordinate(touch.client_y()),
                )
            };

            match event.type_().as_str() {
                "touchstart" => {
                    if primary_touch.is_none()
                        && let Some(touch) = event.changed_touches().get(0)
                    {
                        primary_touch = Some(touch.identifier());
                        let (x, y) = position(&touch);
                        queue_clone.update(x, y, true);
                    }
                }
                "touchmove" => {
                    if let Some(touch) = primary_touch.and_then(|id| find(event.touches(), id)) {
                        let (x, y) = position(&touch);
                        queue_clone.update(x, y, true);
                    }
                }
                // touchend, touchcancel: release where the primary finger was lifted.
                _ => {
                    if let Some(touch) =
                        primary_touch.and_then(|id| find(event.changed_touches(), id))
                    {
                        let (x, y) = position(&touch);
                        queue_clone.update(x, y, false);
                        primary_touch = None;
                    }
                }
            }
        }) as Box<dyn FnMut(TouchEvent)>);

        for event in ["touchstart", "touchend", "touchmove", "touchcancel"] {
            canvas
                .add_event_listener_with_callback(event, touch_closure.as_ref().unchecked_ref())
                .map_err(|_| format!("Failed to add {event} event listener"))?;
        }
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
    ) -> core::task::Poll<io_error::Result<usize>> {
        file_system::ready(|| {
            let data: &mut InputData = buffer
                .try_into()
                .map_err(|_| io_error::Kind::InvalidArgument)?;

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
    ) -> core::task::Poll<io_error::Result<usize>> {
        file_system::ready(|| Err(io_error::Kind::Unsupported))
    }
}

impl MountOperations for MouseDevice {}

impl DirectCharacterDevice for MouseDevice {}
