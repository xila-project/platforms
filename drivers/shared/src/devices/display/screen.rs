use core::{
    cell::UnsafeCell,
    sync::atomic::{AtomicBool, Ordering},
    task::{Context, Poll},
};

use device::screen::{Area, GET_RESOLUTION, Point, SET_DRAWING_AREA, WAS_RESIZED};
use embedded_hal::digital::OutputPin;
use file_system::{
    ControlCommand, ControlCommandIdentifier, DirectBaseOperations, DirectCharacterDevice,
    IoErrorKind, MountOperations, Result, Size,
};
use mipidsi::{
    Display,
    interface::{Interface, InterfacePixelFormat},
    models::Model,
};
use shared::AnyByLayout;

/// Bytes per pixel in the panel's RGB565 wire format.
const BYTES_PER_PIXEL: usize = 2;

/// A cell that one caller at a time may use, without a critical section, so
/// that interrupts keep running during long transfers. A second, concurrent
/// caller gets [`IoErrorKind::ResourceBusy`].
struct Exclusive<T> {
    busy: AtomicBool,
    value: UnsafeCell<T>,
}

// SAFETY: `busy` gives one caller at a time access to `value`.
unsafe impl<T: Send> Sync for Exclusive<T> {}

impl<T> Exclusive<T> {
    const fn new(value: T) -> Self {
        Self {
            busy: AtomicBool::new(false),
            value: UnsafeCell::new(value),
        }
    }

    fn with<R>(&self, function: impl FnOnce(&mut T) -> Result<R>) -> Result<R> {
        if self.busy.swap(true, Ordering::Acquire) {
            return Err(IoErrorKind::ResourceBusy);
        }
        // SAFETY: `busy` was clear, so this is the only reference until it is
        // cleared again below.
        let result = function(unsafe { &mut *self.value.get() });
        self.busy.store(false, Ordering::Release);
        result
    }
}

struct State<DI: Interface, M: Model, RST: OutputPin>
where
    M::ColorFormat: InterfacePixelFormat<DI::Word>,
{
    display: Display<DI, M, RST>,
    area: Area,
}

/// A Xila screen device over a `mipidsi` display whose interface takes RGB565
/// in the panel's wire byte order.
///
/// LVGL renders in that byte order (`RGB565_SWAPPED` on little-endian chips),
/// so each flush is one `set_pixels_raw` call: one address window and one
/// write of LVGL's buffer.
pub struct MipidsiScreenDevice<DI: Interface, M: Model, RST: OutputPin>
where
    M::ColorFormat: InterfacePixelFormat<DI::Word>,
{
    resolution: Point,
    state: Exclusive<State<DI, M, RST>>,
}

impl<DI, M, RST> MipidsiScreenDevice<DI, M, RST>
where
    DI: Interface<Word = u8>,
    M: Model,
    M::ColorFormat: InterfacePixelFormat<u8>,
    RST: OutputPin,
{
    /// Wraps an initialised display of `resolution` pixels, in its current
    /// orientation.
    pub fn new(display: Display<DI, M, RST>, resolution: Point) -> Self {
        Self {
            resolution,
            state: Exclusive::new(State {
                display,
                area: Area::default(),
            }),
        }
    }

    fn check_area(&self, area: &Area) -> Result<()> {
        let (start, end) = (area.get_point_1(), area.get_point_2());
        let inside = |point: Point| {
            (0..self.resolution.get_x()).contains(&point.get_x())
                && (0..self.resolution.get_y()).contains(&point.get_y())
        };
        if inside(start)
            && inside(end)
            && start.get_x() <= end.get_x()
            && start.get_y() <= end.get_y()
        {
            Ok(())
        } else {
            Err(IoErrorKind::InvalidArgument)
        }
    }
}

impl<DI, M, RST> DirectBaseOperations for MipidsiScreenDevice<DI, M, RST>
where
    DI: Interface<Word = u8> + Send,
    M: Model + Send,
    M::ColorFormat: InterfacePixelFormat<u8>,
    RST: OutputPin + Send,
{
    fn read(&self, _: &mut Context<'_>, _: &mut [u8], _: Size) -> Poll<Result<usize>> {
        Poll::Ready(Err(IoErrorKind::Unsupported))
    }

    fn write(&self, _: &mut Context<'_>, buffer: &[u8], _: Size) -> Poll<Result<usize>> {
        file_system::ready(|| {
            self.state.with(|state| {
                let (start, end) = (state.area.get_point_1(), state.area.get_point_2());
                let pixels =
                    usize::from(state.area.get_width()) * usize::from(state.area.get_height());
                let data = buffer
                    .get(..pixels * BYTES_PER_PIXEL)
                    .ok_or(IoErrorKind::InvalidArgument)?;
                // `check_area` keeps the corners inside the screen, so they
                // are non-negative.
                state
                    .display
                    .set_pixels_raw(
                        start.get_x() as u16,
                        start.get_y() as u16,
                        end.get_x() as u16,
                        end.get_y() as u16,
                        data,
                    )
                    .map_err(|_| IoErrorKind::InputOutput)?;
                Ok(buffer.len())
            })
        })
    }

    fn control(
        &self,
        _: &mut Context<'_>,
        command: ControlCommandIdentifier,
        input: &AnyByLayout,
        output: &mut AnyByLayout,
    ) -> Poll<Result<()>> {
        file_system::ready(|| match command {
            SET_DRAWING_AREA::IDENTIFIER => {
                let area = *SET_DRAWING_AREA::cast_input(input)?;
                self.check_area(&area)?;
                self.state.with(|state| {
                    state.area = area;
                    Ok(())
                })
            }
            GET_RESOLUTION::IDENTIFIER => {
                *GET_RESOLUTION::cast_output(output)? = self.resolution;
                Ok(())
            }
            WAS_RESIZED::IDENTIFIER => {
                *WAS_RESIZED::cast_output(output)? = false;
                Ok(())
            }
            _ => Err(IoErrorKind::Unsupported),
        })
    }
}

impl<DI, M, RST> MountOperations for MipidsiScreenDevice<DI, M, RST>
where
    DI: Interface<Word = u8> + Send,
    M: Model + Send,
    M::ColorFormat: InterfacePixelFormat<u8>,
    RST: OutputPin + Send,
{
}

impl<DI, M, RST> DirectCharacterDevice for MipidsiScreenDevice<DI, M, RST>
where
    DI: Interface<Word = u8> + Send,
    M: Model + Send,
    M::ColorFormat: InterfacePixelFormat<u8>,
    RST: OutputPin + Send,
{
}
