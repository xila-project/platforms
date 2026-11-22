extern crate std;

use std::{rc::Rc, vec, vec::Vec};

use core::cell::RefCell;

use device::screen::{Area, GET_RESOLUTION, Point, SET_DRAWING_AREA};
use embedded_hal::delay::DelayNs;
use file_system::{ControlCommand, DirectBaseOperations, IoErrorKind};
use mipidsi::{
    Builder, NoResetPin,
    interface::Interface,
    models::ST7796,
    options::{ColorOrder, Orientation, Rotation},
};
use shared::AnyByLayout;

use super::{MipidsiScreenDevice, ParallelBus, ParallelInterface, STAGING_SIZE};

/// One bus write: the command, a copy of the data and where the data was.
#[derive(Clone, Debug, PartialEq)]
struct Write {
    command: Option<u8>,
    data: Vec<u8>,
    address: usize,
}

/// Records every write. Clones share the log, so a test can read it while
/// the display owns the bus.
#[derive(Clone, Default)]
struct RecordingBus(Rc<RefCell<Vec<Write>>>);

impl RecordingBus {
    fn take(&self) -> Vec<Write> {
        core::mem::take(&mut self.0.borrow_mut())
    }
}

// SAFETY: the tests use the bus on one thread.
unsafe impl Send for RecordingBus {}

impl ParallelBus for RecordingBus {
    type Error = core::convert::Infallible;

    fn write(&mut self, command: Option<u8>, data: &[u8]) -> Result<(), Self::Error> {
        self.0.borrow_mut().push(Write {
            command,
            data: data.to_vec(),
            address: data.as_ptr() as usize,
        });
        Ok(())
    }
}

struct NoDelay;

impl DelayNs for NoDelay {
    fn delay_ns(&mut self, _: u32) {}
}

fn commands(writes: &[Write]) -> Vec<(Option<u8>, Vec<u8>)> {
    writes
        .iter()
        .map(|write| (write.command, write.data.clone()))
        .collect()
}

/// The WT32-SC01 Plus setup: a 320 × 480 panel shown in landscape.
fn landscape_display(
    bus: RecordingBus,
) -> mipidsi::Display<ParallelInterface<RecordingBus>, ST7796, NoResetPin> {
    Builder::new(ST7796, ParallelInterface::new(bus))
        .display_size(320, 480)
        .orientation(Orientation {
            rotation: Rotation::Deg90,
            mirrored: true,
        })
        .color_order(ColorOrder::Bgr)
        .invert_colors(mipidsi::options::ColorInversion::Inverted)
        .init(&mut NoDelay)
        .unwrap()
}

#[test]
fn commands_carry_their_parameters_in_one_write() {
    let bus = RecordingBus::default();
    let mut interface = ParallelInterface::new(bus.clone());
    interface.send_command(0x3A, &[0x55]).unwrap();
    interface.send_command(0x29, &[]).unwrap();
    assert_eq!(
        commands(&bus.take()),
        vec![(Some(0x3A), vec![0x55]), (Some(0x29), vec![])]
    );
}

#[test]
fn raw_pixels_are_one_write_of_the_callers_slice() {
    let bus = RecordingBus::default();
    let mut interface = ParallelInterface::new(bus.clone());
    let data = vec![0xA5; 3 * STAGING_SIZE + 7];
    interface.send_raw_pixels(&data).unwrap();
    let writes = bus.take();
    assert_eq!(writes.len(), 1);
    assert_eq!(writes[0].command, None);
    assert_eq!(writes[0].data, data);
    // Not copied: the bus sees the caller's buffer.
    assert_eq!(writes[0].address, data.as_ptr() as usize);
}

#[test]
fn empty_raw_pixels_write_nothing() {
    let bus = RecordingBus::default();
    let mut interface = ParallelInterface::new(bus.clone());
    interface.send_raw_pixels(&[]).unwrap();
    assert!(bus.take().is_empty());
}

#[test]
fn converted_pixels_are_staged_in_bounded_writes() {
    let bus = RecordingBus::default();
    let mut interface = ParallelInterface::new(bus.clone());
    interface.send_repeated_pixel([0x12, 0x34], 300).unwrap();
    let writes = bus.take();
    let lengths: Vec<usize> = writes.iter().map(|write| write.data.len()).collect();
    assert_eq!(lengths, vec![STAGING_SIZE, 600 - STAGING_SIZE]);
    assert!(writes.iter().all(|write| write.command.is_none()));
    assert!(
        writes
            .iter()
            .flat_map(|write| write.data.chunks(2))
            .all(|pixel| pixel == [0x12, 0x34])
    );
}

#[test]
fn initialisation_goes_through_the_adapter() {
    let bus = RecordingBus::default();
    let _display = landscape_display(bus.clone());
    let writes = commands(&bus.take());
    // Every initialisation step is a command; none sends pixels.
    assert!(writes.iter().all(|(command, _)| command.is_some()));
    let find = |command: u8| {
        writes
            .iter()
            .find(|(written, _)| *written == Some(command))
            .map(|(_, parameters)| parameters.clone())
    };
    assert_eq!(find(0x11), Some(vec![])); // Sleep out.
    assert_eq!(find(0x21), Some(vec![])); // Inversion on.
    assert_eq!(find(0x3A), Some(vec![0x55])); // 16 bits per pixel.
    assert_eq!(find(0x29), Some(vec![])); // Display on.
    // MADCTL: rows and columns swapped (MV, 0x20) with BGR order (0x08), not
    // mirrored, for a landscape 480 × 320 screen.
    assert_eq!(find(0x36), Some(vec![0x28]));
}

#[test]
fn set_pixels_raw_is_one_window_and_one_transfer() {
    let bus = RecordingBus::default();
    let mut display = landscape_display(bus.clone());
    bus.take();
    let data = vec![0x5A; 3 * 2 * 2];
    display.set_pixels_raw(10, 20, 12, 21, &data).unwrap();
    let writes = bus.take();
    assert_eq!(
        commands(&writes),
        vec![
            (Some(0x2A), vec![0, 10, 0, 12]),
            (Some(0x2B), vec![0, 20, 0, 21]),
            (Some(0x2C), vec![]),
            (None, data.clone()),
        ]
    );
    assert_eq!(writes[3].address, data.as_ptr() as usize);
}

fn screen(
    bus: RecordingBus,
) -> MipidsiScreenDevice<ParallelInterface<RecordingBus>, ST7796, NoResetPin> {
    MipidsiScreenDevice::new(landscape_display(bus), Point::new(480, 320))
}

fn set_area(
    screen: &MipidsiScreenDevice<ParallelInterface<RecordingBus>, ST7796, NoResetPin>,
    area: Area,
) -> file_system::Result<()> {
    let mut area = area;
    screen.control_sync(
        SET_DRAWING_AREA::IDENTIFIER,
        AnyByLayout::from_mutable(&mut area),
        AnyByLayout::NONE,
    )
}

#[test]
fn screen_flush_writes_the_drawing_area() {
    let bus = RecordingBus::default();
    let screen = screen(bus.clone());
    bus.take();
    set_area(
        &screen,
        Area::new(Point::new(470, 310), Point::new(479, 319)),
    )
    .unwrap();
    let pixels = vec![0x11; 10 * 10 * 2];
    assert_eq!(screen.write_sync(&pixels, 0), Ok(pixels.len()));
    let writes = bus.take();
    assert_eq!(
        commands(&writes[..2]),
        vec![
            (Some(0x2A), vec![0x01, 0xD6, 0x01, 0xDF]),
            (Some(0x2B), vec![0x01, 0x36, 0x01, 0x3F]),
        ]
    );
    assert_eq!(writes.len(), 4);
    assert_eq!(writes[3].address, pixels.as_ptr() as usize);
}

#[test]
fn screen_rejects_areas_outside_the_screen() {
    let screen = screen(RecordingBus::default());
    for area in [
        Area::new(Point::new(0, 0), Point::new(480, 10)),
        Area::new(Point::new(-1, 0), Point::new(10, 10)),
        Area::new(Point::new(20, 0), Point::new(10, 10)),
    ] {
        assert_eq!(set_area(&screen, area), Err(IoErrorKind::InvalidArgument));
    }
}

#[test]
fn screen_rejects_a_short_buffer() {
    let screen = screen(RecordingBus::default());
    set_area(&screen, Area::new(Point::new(0, 0), Point::new(9, 9))).unwrap();
    assert_eq!(
        screen.write_sync(&[0; 10], 0),
        Err(IoErrorKind::InvalidArgument)
    );
}

#[test]
fn screen_reports_its_resolution() {
    let screen = screen(RecordingBus::default());
    let mut resolution = Point::new(0, 0);
    screen
        .control_sync(
            GET_RESOLUTION::IDENTIFIER,
            AnyByLayout::NONE,
            AnyByLayout::from_mutable(&mut resolution),
        )
        .unwrap();
    assert_eq!(resolution, Point::new(480, 320));
}

/// Fails every pixel write, as a bus whose DMA stopped would.
struct FailingBus {
    writes: Rc<RefCell<usize>>,
}

// SAFETY: the tests use the bus on one thread.
unsafe impl Send for FailingBus {}

impl ParallelBus for FailingBus {
    type Error = ();

    fn write(&mut self, command: Option<u8>, _: &[u8]) -> Result<(), Self::Error> {
        *self.writes.borrow_mut() += 1;
        // Commands pass, so that the panel initialises; pixel data fails.
        if command.is_some() { Ok(()) } else { Err(()) }
    }
}

#[test]
fn a_failed_transfer_drops_frames_instead_of_failing_the_flush() {
    let writes = Rc::new(RefCell::new(0));
    let display = Builder::new(
        ST7796,
        ParallelInterface::new(FailingBus {
            writes: writes.clone(),
        }),
    )
    .display_size(320, 480)
    .init(&mut NoDelay)
    .unwrap();
    let screen = MipidsiScreenDevice::new(display, Point::new(320, 480));
    let mut area = Area::new(Point::new(0, 0), Point::new(9, 9));
    screen
        .control_sync(
            SET_DRAWING_AREA::IDENTIFIER,
            AnyByLayout::from_mutable(&mut area),
            AnyByLayout::NONE,
        )
        .unwrap();
    let pixels = vec![0; 10 * 10 * 2];
    assert!(!screen.has_failed());
    assert_eq!(screen.write_sync(&pixels, 0), Ok(pixels.len()));
    assert!(screen.has_failed());
    // Later frames do not reach the bus.
    let writes_after_failure = *writes.borrow();
    assert_eq!(screen.write_sync(&pixels, 0), Ok(pixels.len()));
    assert_eq!(*writes.borrow(), writes_after_failure);
}
