extern crate std;

use std::{boxed::Box, rc::Rc, vec, vec::Vec};

use core::cell::RefCell;

use device::screen::{
    Area, BRIGHTNESS_FULL, BRIGHTNESS_OFF, GET_BRIGHTNESS, GET_RESOLUTION, Point, SET_BRIGHTNESS,
    SET_DRAWING_AREA,
};
use embedded_hal::delay::DelayNs;
use file_system::{ControlCommand, DirectBaseOperations, DirectCharacterDevice, IoErrorKind};
use mipidsi::{
    Builder, NoResetPin,
    interface::Interface,
    models::ST7796,
    options::{ColorOrder, Orientation, Rotation},
};
use shared::AnyByLayout;

use super::{
    BacklightScreenDevice, MipidsiScreenDevice, ParallelBus, ParallelInterface, STAGING_SIZE,
    continuation_command,
};

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
    // No memory write start came first: the pixels continue a memory write.
    assert_eq!(writes[0].command, Some(0x3C));
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
    assert!(writes.iter().all(|write| write.command == Some(0x3C)));
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
            // The memory write start and the pixels are one transaction.
            (Some(0x2C), data.clone()),
        ]
    );
    assert_eq!(writes[2].address, data.as_ptr() as usize);
}

#[test]
fn every_write_of_pixels_carries_a_memory_write_command() {
    let bus = RecordingBus::default();
    let mut interface = ParallelInterface::new(bus.clone());
    interface.send_command(0x2C, &[]).unwrap();
    // The start is held back until pixels follow.
    assert!(bus.take().is_empty());
    interface.send_repeated_pixel([0x12, 0x34], 300).unwrap();
    interface.send_raw_pixels(&[0x56, 0x78]).unwrap();
    let written: Vec<(Option<u8>, usize)> = bus
        .take()
        .iter()
        .map(|write| (write.command, write.data.len()))
        .collect();
    assert_eq!(
        written,
        vec![
            (Some(0x2C), STAGING_SIZE),
            (Some(0x3C), 600 - STAGING_SIZE),
            (Some(0x3C), 2),
        ]
    );
}

#[test]
fn a_memory_write_start_without_pixels_keeps_its_place() {
    let bus = RecordingBus::default();
    let mut interface = ParallelInterface::new(bus.clone());
    interface.send_command(0x2C, &[]).unwrap();
    interface.send_command(0x29, &[]).unwrap();
    assert_eq!(
        commands(&bus.take()),
        vec![(Some(0x2C), vec![]), (Some(0x29), vec![])]
    );
}

#[test]
fn two_memory_write_starts_without_pixels_both_go_out() {
    let bus = RecordingBus::default();
    let mut interface = ParallelInterface::new(bus.clone());
    interface.send_command(0x2C, &[]).unwrap();
    interface.send_command(0x2C, &[]).unwrap();
    interface.send_raw_pixels(&[0x12, 0x34]).unwrap();
    assert_eq!(
        commands(&bus.take()),
        vec![(Some(0x2C), vec![]), (Some(0x2C), vec![0x12, 0x34])]
    );
}

#[test]
fn a_memory_write_start_keeps_its_place_around_empty_pixel_writes() {
    let bus = RecordingBus::default();
    let mut interface = ParallelInterface::new(bus.clone());
    interface.send_command(0x2C, &[]).unwrap();
    interface.send_raw_pixels(&[]).unwrap();
    interface.send_pixels::<2>(core::iter::empty()).unwrap();
    assert!(bus.take().is_empty());
    interface.send_command(0x29, &[]).unwrap();
    assert_eq!(
        commands(&bus.take()),
        vec![(Some(0x2C), vec![]), (Some(0x29), vec![])]
    );
}

#[test]
fn every_other_command_goes_out_unchanged() {
    let bus = RecordingBus::default();
    let mut interface = ParallelInterface::new(bus.clone());
    let sent: Vec<(u8, Vec<u8>)> = vec![
        (0x11, vec![]),
        (0x2A, vec![0, 1, 0, 2]),
        (0x2B, vec![0, 3, 0, 4]),
        (0x36, vec![0x20]),
        (0x3A, vec![0x55]),
        (0x3C, vec![]),
        // A memory write start with parameters is not held back.
        (0x2C, vec![0x01]),
        (0x29, vec![]),
    ];
    for (command, arguments) in &sent {
        interface.send_command(*command, arguments).unwrap();
    }
    let expected: Vec<(Option<u8>, Vec<u8>)> = sent
        .into_iter()
        .map(|(command, arguments)| (Some(command), arguments))
        .collect();
    assert_eq!(commands(&bus.take()), expected);
}

#[test]
fn each_window_starts_with_a_memory_write_start() {
    let bus = RecordingBus::default();
    let mut display = landscape_display(bus.clone());
    bus.take();
    let data = vec![0x12; 30 * 10 * 2];
    display.set_pixels_raw(0, 0, 29, 9, &data).unwrap();
    display.set_pixels_raw(0, 0, 29, 9, &data).unwrap();
    // Per window: the address window, then the pixels with the start command.
    // A memory write continue never follows an address window.
    let written: Vec<(Option<u8>, usize)> = bus
        .take()
        .iter()
        .map(|write| (write.command, write.data.len()))
        .collect();
    assert_eq!(
        written,
        vec![
            (Some(0x2A), 4),
            (Some(0x2B), 4),
            (Some(0x2C), data.len()),
            (Some(0x2A), 4),
            (Some(0x2B), 4),
            (Some(0x2C), data.len()),
        ]
    );
}

#[test]
fn a_bus_error_does_not_leave_a_held_back_start() {
    // The first memory write fails; the next window works from its own start.
    struct FailOnce {
        log: Rc<RefCell<Vec<Option<u8>>>>,
        failed: bool,
    }
    // SAFETY: the tests use the bus on one thread.
    unsafe impl Send for FailOnce {}
    impl ParallelBus for FailOnce {
        type Error = ();

        fn write(&mut self, command: Option<u8>, _: &[u8]) -> Result<(), Self::Error> {
            self.log.borrow_mut().push(command);
            if command == Some(0x2C) && !self.failed {
                self.failed = true;
                return Err(());
            }
            Ok(())
        }
    }
    let log = Rc::new(RefCell::new(Vec::new()));
    let mut interface = ParallelInterface::new(FailOnce {
        log: log.clone(),
        failed: false,
    });
    interface.send_command(0x2C, &[]).unwrap();
    assert_eq!(interface.send_raw_pixels(&[1, 2]), Err(()));
    log.borrow_mut().clear();
    interface.send_command(0x2A, &[0, 0, 0, 1]).unwrap();
    interface.send_command(0x2C, &[]).unwrap();
    interface.send_raw_pixels(&[3, 4]).unwrap();
    assert_eq!(*log.borrow(), vec![Some(0x2A), Some(0x2C)]);
}

#[test]
fn bus_continuation_commands() {
    assert_eq!(continuation_command(Some(0x2C)), Some(0x3C));
    assert_eq!(continuation_command(Some(0x3C)), Some(0x3C));
    assert_eq!(continuation_command(Some(0x2A)), None);
    assert_eq!(continuation_command(None), None);
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
    assert_eq!(writes.len(), 3);
    assert_eq!(writes[2].command, Some(0x2C));
    assert_eq!(writes[2].address, pixels.as_ptr() as usize);
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
        match command {
            Some(0x2C | 0x3C) => Err(()),
            _ => Ok(()),
        }
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

#[derive(Debug, PartialEq, Eq)]
struct WindowFlush {
    bounds: (u16, u16, u16, u16),
    address: usize,
    length: usize,
}

#[derive(Default)]
struct MockAsynchronousState {
    commands: Vec<(u8, Vec<u8>)>,
    sync_pixels: Vec<Vec<u8>>,
    window_flushes: Vec<WindowFlush>,
    in_flight: bool,
    waits: usize,
    fail_window: bool,
}

#[derive(Clone, Default)]
struct MockAsynchronousBus(Rc<RefCell<MockAsynchronousState>>);

// SAFETY: tests drive the mock bus on one thread.
unsafe impl Send for MockAsynchronousBus {}

impl super::AsynchronousDisplayBus for MockAsynchronousBus {
    type Error = ();

    fn send_command(&mut self, command: u8, arguments: &[u8]) -> Result<(), Self::Error> {
        let mut state = self.0.borrow_mut();
        if state.in_flight {
            state.in_flight = false;
            state.waits += 1;
        }
        state.commands.push((command, arguments.to_vec()));
        Ok(())
    }

    fn send_pixels(&mut self, data: &[u8]) -> Result<(), Self::Error> {
        let mut state = self.0.borrow_mut();
        if state.in_flight {
            state.in_flight = false;
            state.waits += 1;
        }
        state.sync_pixels.push(data.to_vec());
        Ok(())
    }

    unsafe fn start_window_pixels(
        &mut self,
        x_start: u16,
        y_start: u16,
        x_end: u16,
        y_end: u16,
        data: &[u8],
    ) -> Result<bool, Self::Error> {
        let mut state = self.0.borrow_mut();
        if state.in_flight {
            state.in_flight = false;
            state.waits += 1;
        }
        if state.fail_window {
            return Err(());
        }
        state.window_flushes.push(WindowFlush {
            bounds: (x_start, y_start, x_end, y_end),
            address: data.as_ptr() as usize,
            length: data.len(),
        });
        state.in_flight = true;
        Ok(true)
    }

    fn wait_for_transfer(&mut self) -> Result<(), Self::Error> {
        let mut state = self.0.borrow_mut();
        if state.in_flight {
            state.in_flight = false;
            state.waits += 1;
        }
        Ok(())
    }
}

#[test]
fn serial_interface_initialises_st7796_and_releases_bus() {
    let bus = MockAsynchronousBus::default();
    let display = Builder::new(ST7796, super::SerialInterface::new(bus.clone()))
        .display_size(320, 480)
        .orientation(Orientation {
            rotation: Rotation::Deg90,
            mirrored: true,
        })
        .color_order(ColorOrder::Bgr)
        .init(&mut NoDelay)
        .unwrap();
    let (interface, _, _) = display.release();
    let _bus = interface.release();
    let commands = &bus.0.borrow().commands;
    assert!(commands.iter().any(|(command, _)| *command == 0x29));
    assert!(
        commands
            .iter()
            .any(|(command, arguments)| *command == 0x36 && arguments == &[0x28])
    );
}

#[test]
fn asynchronous_screen_device_overlaps_and_waits_cleanly() {
    let bus = MockAsynchronousBus::default();
    let screen = super::AsynchronousScreenDevice::new(bus.clone(), (), Point::new(480, 320));
    let buffer_one = vec![0xAA; 480 * 20 * 2];
    let buffer_two = vec![0x55; 480 * 20 * 2];

    let pending_one = unsafe {
        screen.start_flush(
            Area::new(Point::new(0, 0), Point::new(479, 19)),
            &buffer_one,
        )
    };
    assert!(pending_one);
    assert!(bus.0.borrow().in_flight);
    assert_eq!(bus.0.borrow().waits, 0);

    screen.wait_for_flush();
    assert!(!bus.0.borrow().in_flight);
    assert_eq!(bus.0.borrow().waits, 1);

    let pending_two = unsafe {
        screen.start_flush(
            Area::new(Point::new(0, 20), Point::new(479, 39)),
            &buffer_two,
        )
    };
    assert!(pending_two);
    assert_eq!(
        bus.0.borrow().window_flushes,
        vec![
            WindowFlush {
                bounds: (0, 0, 479, 19),
                address: buffer_one.as_ptr() as usize,
                length: buffer_one.len(),
            },
            WindowFlush {
                bounds: (0, 20, 479, 39),
                address: buffer_two.as_ptr() as usize,
                length: buffer_two.len(),
            },
        ]
    );
    screen.wait_for_flush();
    assert_eq!(bus.0.borrow().waits, 2);
}

#[test]
fn asynchronous_screen_device_synchronous_write_waits_before_returning() {
    let bus = MockAsynchronousBus::default();
    let screen = super::AsynchronousScreenDevice::new(bus.clone(), (), Point::new(480, 320));
    let mut area = Area::new(Point::new(0, 0), Point::new(9, 9));
    screen
        .control_sync(
            SET_DRAWING_AREA::IDENTIFIER,
            AnyByLayout::from_mutable(&mut area),
            AnyByLayout::NONE,
        )
        .unwrap();
    let pixels = vec![0x22; 10 * 10 * 2];
    assert_eq!(screen.write_sync(&pixels, 0), Ok(pixels.len()));
    assert!(!bus.0.borrow().in_flight);
    assert_eq!(bus.0.borrow().waits, 1);
}

#[test]
fn asynchronous_screen_device_failure_marks_failed_and_clears_pending() {
    let bus = MockAsynchronousBus::default();
    bus.0.borrow_mut().fail_window = true;
    let screen = super::AsynchronousScreenDevice::new(bus, (), Point::new(480, 320));
    let pixels = vec![0; 10 * 10 * 2];
    let pending =
        unsafe { screen.start_flush(Area::new(Point::new(0, 0), Point::new(9, 9)), &pixels) };
    assert!(!pending);
    assert!(screen.has_failed());
}

#[test]
fn display_performance_reports_after_window_and_formats_summary() {
    use std::string::ToString;

    let mut tracker = super::DisplayPerformance::new(1_000_000);
    // Two frames across 1.0 second: 20 ms and 40 ms total, 2 flushes each.
    // The first redraws a button that was invalidated twice, the second a
    // list and a label inside it.
    let mut named = Vec::new();
    let mut name = |class: &'static str| {
        named.push(class);
        Some(class)
    };
    tracker.on_invalidate(4_000, || name("lv_button"));
    tracker.on_invalidate(4_000, || name("lv_button"));
    tracker.on_render_start(100_000);
    tracker.on_flush_start(105_000, 2_000);
    tracker.on_flush_finish(106_000);
    tracker.on_wait_start(110_000);
    tracker.on_wait_finish(118_000);
    tracker.on_flush_start(118_000, 2_000);
    tracker.on_flush_finish(119_000);
    assert_eq!(tracker.on_render_ready(120_000), None);

    tracker.on_invalidate(96_000, || name("lv_list"));
    tracker.on_invalidate(1_000, || name("lv_label"));
    tracker.on_render_start(1_060_000);
    tracker.on_flush_start(1_070_000, 9_600);
    tracker.on_flush_finish(1_072_000);
    tracker.on_wait_start(1_080_000);
    tracker.on_wait_finish(1_096_000);
    tracker.on_flush_start(1_096_000, 86_400);
    tracker.on_flush_finish(1_098_000);
    let report = tracker
        .on_render_ready(1_100_000)
        .expect("window elapsed after one second");

    assert_eq!(report.frames, 2);
    assert_eq!(report.flushes, 4);
    assert_eq!(report.elapsed_microseconds, 1_000_000);
    assert_eq!(report.frames_per_second_tenths(), 20);
    assert_eq!(report.frame_average_microseconds, 30_000);
    assert_eq!(report.frame_maximum_microseconds, 40_000);
    assert_eq!(report.flush_average_microseconds, 3_000);
    assert_eq!(report.wait_average_microseconds, 12_000);
    assert_eq!(report.invalidations_average_tenths, 20);
    assert_eq!(report.invalidated_pixels_average, 52_500);
    assert_eq!(report.flushed_pixels_average, 50_000);
    assert_eq!(
        report.largest_invalidation,
        Some(super::Invalidation {
            pixels: 96_000,
            class: Some("lv_list"),
        })
    );
    // Only an area larger than the ones before it is looked up.
    assert_eq!(named, ["lv_button", "lv_list"]);
    assert_eq!(
        report.to_string(),
        "Display performance: 2.0 frames/s (2 frames, 4 flushes, frame average 30.0 ms, maximum 40.0 ms, flush average 3.0 ms, wait average 12.0 ms; per frame 2.0 invalidations of 52500 pixels, 50000 pixels flushed; largest invalidation 96000 pixels by lv_list)"
    );

    // The next window starts from nothing.
    tracker.on_invalidate(300, || None);
    tracker.on_render_start(2_090_000);
    tracker.on_flush_start(2_091_000, 300);
    tracker.on_flush_finish(2_092_000);
    let report = tracker.on_render_ready(2_100_000).unwrap();
    assert_eq!(report.invalidations_average_tenths, 10);
    assert_eq!(report.flushed_pixels_average, 300);
    assert!(
        report
            .to_string()
            .ends_with("largest invalidation 300 pixels by an unknown object)")
    );
}

/// A 10-bit PWM output, as the ESP32 LED controller sets one up, whose duty
/// cycle the test reads.
#[derive(Clone, Default)]
struct RecordingPwm(Rc<RefCell<u16>>);

// SAFETY: the tests use the output on one thread.
unsafe impl Send for RecordingPwm {}

impl embedded_hal::pwm::ErrorType for RecordingPwm {
    type Error = core::convert::Infallible;
}

impl embedded_hal::pwm::SetDutyCycle for RecordingPwm {
    fn max_duty_cycle(&self) -> u16 {
        1024
    }

    fn set_duty_cycle(&mut self, duty: u16) -> Result<(), Self::Error> {
        *self.0.borrow_mut() = duty;
        Ok(())
    }
}

fn brightness(screen: &dyn DirectBaseOperations) -> Result<u8, IoErrorKind> {
    let mut level = 0u8;
    screen.control_sync(
        GET_BRIGHTNESS::IDENTIFIER,
        AnyByLayout::NONE,
        AnyByLayout::from_mutable(&mut level),
    )?;
    Ok(level)
}

fn set_brightness(screen: &dyn DirectBaseOperations, mut level: u8) -> Result<(), IoErrorKind> {
    screen.control_sync(
        SET_BRIGHTNESS::IDENTIFIER,
        AnyByLayout::from_mutable(&mut level),
        AnyByLayout::NONE,
    )
}

#[test]
fn backlight_follows_the_brightness_level_and_starts_fully_on() {
    let inner: &'static dyn DirectCharacterDevice =
        Box::leak(Box::new(screen(RecordingBus::default())));
    // The panel alone has no brightness control.
    assert_eq!(brightness(inner), Err(IoErrorKind::Unsupported));
    assert_eq!(set_brightness(inner, 10), Err(IoErrorKind::Unsupported));

    let pwm = RecordingPwm::default();
    let screen = BacklightScreenDevice::new(inner, pwm.clone()).unwrap();
    assert_eq!(*pwm.0.borrow(), 1024);
    assert_eq!(brightness(&screen), Ok(BRIGHTNESS_FULL));

    set_brightness(&screen, 128).unwrap();
    assert_eq!(*pwm.0.borrow(), 514);
    assert_eq!(brightness(&screen), Ok(128));
    set_brightness(&screen, BRIGHTNESS_OFF).unwrap();
    assert_eq!(*pwm.0.borrow(), 0);
    set_brightness(&screen, BRIGHTNESS_FULL).unwrap();
    assert_eq!(*pwm.0.borrow(), 1024);

    // Every other command reaches the panel.
    let mut resolution = Point::new(0, 0);
    screen
        .control_sync(
            GET_RESOLUTION::IDENTIFIER,
            AnyByLayout::NONE,
            AnyByLayout::from_mutable(&mut resolution),
        )
        .unwrap();
    assert_eq!(resolution, Point::new(480, 320));
    let mut area = Area::new(Point::new(0, 0), Point::new(1, 0));
    screen
        .control_sync(
            SET_DRAWING_AREA::IDENTIFIER,
            AnyByLayout::from_mutable(&mut area),
            AnyByLayout::NONE,
        )
        .unwrap();
    assert_eq!(screen.write_sync(&[0; 4], 0), Ok(4));
}
