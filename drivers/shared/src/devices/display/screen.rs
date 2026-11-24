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

struct State<DisplayInterface: Interface, PanelModel: Model, ResetPin: OutputPin>
where
    PanelModel::ColorFormat: InterfacePixelFormat<DisplayInterface::Word>,
{
    display: Display<DisplayInterface, PanelModel, ResetPin>,
    area: Area,
}

/// A Xila screen device over a `mipidsi` display whose interface takes RGB565
/// in the panel's wire byte order.
///
/// LVGL renders in that byte order (`RGB565_SWAPPED` on little-endian chips),
/// so each flush is one `set_pixels_raw` call: one address window and one
/// write of LVGL's buffer.
pub struct MipidsiScreenDevice<DisplayInterface: Interface, PanelModel: Model, ResetPin: OutputPin>
where
    PanelModel::ColorFormat: InterfacePixelFormat<DisplayInterface::Word>,
{
    resolution: Point,
    state: Exclusive<State<DisplayInterface, PanelModel, ResetPin>>,
    /// Set after a transfer failed: later frames are dropped.
    failed: AtomicBool,
}

impl<DisplayInterface, PanelModel, ResetPin>
    MipidsiScreenDevice<DisplayInterface, PanelModel, ResetPin>
where
    DisplayInterface: Interface<Word = u8>,
    PanelModel: Model,
    PanelModel::ColorFormat: InterfacePixelFormat<u8>,
    ResetPin: OutputPin,
{
    /// Wraps an initialised display of `resolution` pixels, in its current
    /// orientation.
    pub fn new(
        display: Display<DisplayInterface, PanelModel, ResetPin>,
        resolution: Point,
    ) -> Self {
        Self {
            resolution,
            state: Exclusive::new(State {
                display,
                area: Area::default(),
            }),
            failed: AtomicBool::new(false),
        }
    }

    /// Whether a transfer to the panel failed. The device then drops every
    /// frame, so that rendering, and the rest of the system, keep running.
    pub fn has_failed(&self) -> bool {
        self.failed.load(Ordering::Acquire)
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

impl<DisplayInterface, PanelModel, ResetPin> DirectBaseOperations
    for MipidsiScreenDevice<DisplayInterface, PanelModel, ResetPin>
where
    DisplayInterface: Interface<Word = u8> + Send,
    PanelModel: Model + Send,
    PanelModel::ColorFormat: InterfacePixelFormat<u8>,
    ResetPin: OutputPin + Send,
{
    fn read(&self, _: &mut Context<'_>, _: &mut [u8], _: Size) -> Poll<Result<usize>> {
        Poll::Ready(Err(IoErrorKind::Unsupported))
    }

    fn write(&self, _: &mut Context<'_>, buffer: &[u8], _: Size) -> Poll<Result<usize>> {
        file_system::ready(|| {
            if self.has_failed() {
                return Ok(buffer.len());
            }
            self.state.with(|state| {
                let (start, end) = (state.area.get_point_1(), state.area.get_point_2());
                let pixels =
                    usize::from(state.area.get_width()) * usize::from(state.area.get_height());
                let data = buffer
                    .get(..pixels * BYTES_PER_PIXEL)
                    .ok_or(IoErrorKind::InvalidArgument)?;
                // `check_area` keeps the corners inside the screen, so they
                // are non-negative.
                let result = state.display.set_pixels_raw(
                    start.get_x() as u16,
                    start.get_y() as u16,
                    end.get_x() as u16,
                    end.get_y() as u16,
                    data,
                );
                // A failed transfer drops this frame and the next ones rather
                // than failing the flush: the panel stays as it is, and the
                // other sessions keep running.
                if let Err(error) = result {
                    self.failed.store(true, Ordering::Release);
                    log::error!("The screen stopped after a failed transfer: {error:?}");
                }
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

impl<DisplayInterface, PanelModel, ResetPin> MountOperations
    for MipidsiScreenDevice<DisplayInterface, PanelModel, ResetPin>
where
    DisplayInterface: Interface<Word = u8> + Send,
    PanelModel: Model + Send,
    PanelModel::ColorFormat: InterfacePixelFormat<u8>,
    ResetPin: OutputPin + Send,
{
}

impl<DisplayInterface, PanelModel, ResetPin> DirectCharacterDevice
    for MipidsiScreenDevice<DisplayInterface, PanelModel, ResetPin>
where
    DisplayInterface: Interface<Word = u8> + Send,
    PanelModel: Model + Send,
    PanelModel::ColorFormat: InterfacePixelFormat<u8>,
    ResetPin: OutputPin + Send,
{
}

struct AsyncState<Bus> {
    bus: Bus,
    area: Area,
}

/// A Xila screen device backed by an [`super::AsyncDisplayBus`].
///
/// The [`DirectCharacterDevice`] implementation remains synchronous for
/// generic callers, while [`start_flush`](Self::start_flush) and
/// [`wait_for_flush`](Self::wait_for_flush) let LVGL double-buffered flushes
/// overlap DMA transfers with rendering.
pub struct AsyncScreenDevice<Bus, ResetPin> {
    resolution: Point,
    state: Exclusive<AsyncState<Bus>>,
    /// Kept alive for the lifetime of the panel so the pin state is preserved.
    _reset: ResetPin,
    /// Set after a transfer failed: later frames are dropped.
    failed: AtomicBool,
}

impl<Bus: super::AsyncDisplayBus, ResetPin> AsyncScreenDevice<Bus, ResetPin> {
    /// Wraps an already-initialised panel bus and its reset pin.
    pub fn new(bus: Bus, reset: ResetPin, resolution: Point) -> Self {
        Self {
            resolution,
            state: Exclusive::new(AsyncState {
                bus,
                area: Area::default(),
            }),
            _reset: reset,
            failed: AtomicBool::new(false),
        }
    }

    /// Whether a transfer to the panel failed. The device then drops every
    /// frame so rendering and the rest of the system keep running.
    pub fn has_failed(&self) -> bool {
        self.failed.load(Ordering::Acquire)
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

    /// Starts flushing `buffer` to `area`. Returns `true` when an asynchronous
    /// transfer is in flight and [`wait_for_flush`](Self::wait_for_flush) must
    /// be called before reusing `buffer`.
    ///
    /// # Safety
    ///
    /// When this returns `true`, `buffer` must stay valid and unmodified until
    /// [`wait_for_flush`](Self::wait_for_flush) returns.
    pub unsafe fn start_flush(&self, area: Area, buffer: &[u8]) -> bool {
        if self.has_failed() || self.check_area(&area).is_err() {
            return false;
        }
        let pixels = usize::from(area.get_width()) * usize::from(area.get_height());
        let Some(data) = buffer.get(..pixels * BYTES_PER_PIXEL) else {
            return false;
        };
        let (start, end) = (area.get_point_1(), area.get_point_2());
        self.state
            .with(|state| {
                // SAFETY: caller guarantees `data` outlives the transfer until
                // `wait_for_flush` completes.
                let result = unsafe {
                    state.bus.start_window_pixels(
                        start.get_x() as u16,
                        start.get_y() as u16,
                        end.get_x() as u16,
                        end.get_y() as u16,
                        data,
                    )
                };
                match result {
                    Ok(pending) => Ok(pending),
                    Err(error) => {
                        self.failed.store(true, Ordering::Release);
                        log::error!("The screen stopped after a failed transfer: {error:?}");
                        Ok(false)
                    }
                }
            })
            .unwrap_or(false)
    }

    /// Waits for any in-flight asynchronous flush to finish.
    pub fn wait_for_flush(&self) {
        if self.has_failed() {
            return;
        }
        let _ = self.state.with(|state| {
            if let Err(error) = state.bus.wait_for_transfer() {
                self.failed.store(true, Ordering::Release);
                log::error!("The screen stopped after a failed transfer: {error:?}");
            }
            Ok(())
        });
    }
}

impl<Bus, ResetPin> DirectBaseOperations for AsyncScreenDevice<Bus, ResetPin>
where
    Bus: super::AsyncDisplayBus + Send,
    ResetPin: Send + Sync,
{
    fn read(&self, _: &mut Context<'_>, _: &mut [u8], _: Size) -> Poll<Result<usize>> {
        Poll::Ready(Err(IoErrorKind::Unsupported))
    }

    fn write(&self, _: &mut Context<'_>, buffer: &[u8], _: Size) -> Poll<Result<usize>> {
        file_system::ready(|| {
            if self.has_failed() {
                return Ok(buffer.len());
            }
            self.state.with(|state| {
                let (start, end) = (state.area.get_point_1(), state.area.get_point_2());
                let pixels =
                    usize::from(state.area.get_width()) * usize::from(state.area.get_height());
                let data = buffer
                    .get(..pixels * BYTES_PER_PIXEL)
                    .ok_or(IoErrorKind::InvalidArgument)?;
                // SAFETY: `wait_for_transfer` runs immediately below before
                // returning, so `data` outlives the transfer.
                let result = unsafe {
                    state.bus.start_window_pixels(
                        start.get_x() as u16,
                        start.get_y() as u16,
                        end.get_x() as u16,
                        end.get_y() as u16,
                        data,
                    )
                }
                .and_then(|pending| {
                    if pending {
                        state.bus.wait_for_transfer()
                    } else {
                        Ok(())
                    }
                });
                if let Err(error) = result {
                    self.failed.store(true, Ordering::Release);
                    log::error!("The screen stopped after a failed transfer: {error:?}");
                }
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

impl<Bus, ResetPin> MountOperations for AsyncScreenDevice<Bus, ResetPin>
where
    Bus: super::AsyncDisplayBus + Send,
    ResetPin: Send + Sync,
{
}

impl<Bus, ResetPin> DirectCharacterDevice for AsyncScreenDevice<Bus, ResetPin>
where
    Bus: super::AsyncDisplayBus + Send,
    ResetPin: Send + Sync,
{
}
