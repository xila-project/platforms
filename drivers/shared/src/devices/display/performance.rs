//! Lightweight display render and flush performance tracker.
//!
//! Accumulates per-frame render duration, flush callback time, and asynchronous
//! flush wait time over a reporting window so firmware can emit a single
//! summary line over the serial console when debug logging is enabled.
//!
//! It also counts what was invalidated and what was flushed. Rendering time
//! follows the pixels LVGL redraws, and LVGL redraws what was invalidated: these
//! tell a frame that is slow to draw from one that draws more than it needs to.

use core::fmt;

/// Default interval between summary lines, in microseconds (two seconds).
pub const DEFAULT_REPORT_INTERVAL_MICROSECONDS: u64 = 2_000_000;

/// Summary of display activity over one reporting window.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DisplayPerformanceReport {
    /// Length of the reporting window in microseconds.
    pub elapsed_microseconds: u64,
    /// Number of rendered frames in the window.
    pub frames: u32,
    /// Number of LVGL flushes in the window.
    pub flushes: u32,
    /// Average wall time per frame from render start to render ready, in microseconds.
    pub frame_average_microseconds: u64,
    /// Longest single frame in the window, in microseconds.
    pub frame_maximum_microseconds: u64,
    /// Average time per frame spent inside the flush callback, in microseconds.
    pub flush_average_microseconds: u64,
    /// Average time per frame spent waiting for an asynchronous flush to finish, in microseconds.
    pub wait_average_microseconds: u64,
    /// Average number of areas invalidated per frame, in tenths.
    pub invalidations_average_tenths: u64,
    /// Average number of pixels invalidated per frame. Areas that overlap count
    /// once each, so this can exceed the screen.
    pub invalidated_pixels_average: u64,
    /// Average number of pixels flushed per frame: what was actually redrawn,
    /// once LVGL has joined the invalidated areas.
    pub flushed_pixels_average: u64,
    /// The largest area invalidated in the window.
    pub largest_invalidation: Option<Invalidation>,
}

/// One invalidated area.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Invalidation {
    /// Size of the area in pixels.
    pub pixels: u32,
    /// Class of the object the area belongs to, when it is known.
    pub class: Option<&'static str>,
}

impl DisplayPerformanceReport {
    /// Frames per second expressed in tenths (for example, `245` means `24.5`).
    pub fn frames_per_second_tenths(&self) -> u64 {
        (u64::from(self.frames) * 10_000_000) / self.elapsed_microseconds.max(1)
    }
}

fn write_tenths_milliseconds(formatter: &mut fmt::Formatter<'_>, microseconds: u64) -> fmt::Result {
    let tenths = (microseconds + 50) / 100;
    write!(formatter, "{}.{}", tenths / 10, tenths % 10)
}

impl fmt::Display for DisplayPerformanceReport {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let rate_tenths = self.frames_per_second_tenths();
        write!(
            formatter,
            "Display performance: {}.{} frames/s ({} frames, {} flushes, frame average ",
            rate_tenths / 10,
            rate_tenths % 10,
            self.frames,
            self.flushes,
        )?;
        write_tenths_milliseconds(formatter, self.frame_average_microseconds)?;
        formatter.write_str(" ms, maximum ")?;
        write_tenths_milliseconds(formatter, self.frame_maximum_microseconds)?;
        formatter.write_str(" ms, flush average ")?;
        write_tenths_milliseconds(formatter, self.flush_average_microseconds)?;
        formatter.write_str(" ms, wait average ")?;
        write_tenths_milliseconds(formatter, self.wait_average_microseconds)?;
        write!(
            formatter,
            " ms; per frame {}.{} invalidations of {} pixels, {} pixels flushed",
            self.invalidations_average_tenths / 10,
            self.invalidations_average_tenths % 10,
            self.invalidated_pixels_average,
            self.flushed_pixels_average,
        )?;
        if let Some(largest) = self.largest_invalidation {
            write!(
                formatter,
                "; largest invalidation {} pixels by {}",
                largest.pixels,
                largest.class.unwrap_or("an unknown object"),
            )?;
        }
        formatter.write_str(")")
    }
}

/// Accumulates LVGL render and flush timings across frames.
#[derive(Debug)]
pub struct DisplayPerformance {
    report_interval_microseconds: u64,
    window_started_microseconds: Option<u64>,
    frames: u32,
    flushes: u32,
    frame_total_microseconds: u64,
    frame_maximum_microseconds: u64,
    flush_total_microseconds: u64,
    wait_total_microseconds: u64,
    invalidations: u32,
    invalidated_pixels: u64,
    flushed_pixels: u64,
    largest_invalidation: Option<Invalidation>,
    current_frame_start: Option<u64>,
    current_flush_start: Option<u64>,
    current_wait_start: Option<u64>,
}

impl Default for DisplayPerformance {
    fn default() -> Self {
        Self::new(DEFAULT_REPORT_INTERVAL_MICROSECONDS)
    }
}

impl DisplayPerformance {
    /// Creates a tracker that reports after at least `report_interval_microseconds`.
    pub const fn new(report_interval_microseconds: u64) -> Self {
        Self {
            report_interval_microseconds,
            window_started_microseconds: None,
            frames: 0,
            flushes: 0,
            frame_total_microseconds: 0,
            frame_maximum_microseconds: 0,
            flush_total_microseconds: 0,
            wait_total_microseconds: 0,
            invalidations: 0,
            invalidated_pixels: 0,
            flushed_pixels: 0,
            largest_invalidation: None,
            current_frame_start: None,
            current_flush_start: None,
            current_wait_start: None,
        }
    }

    /// Records the start of an LVGL frame render (`LV_EVENT_RENDER_START`).
    pub fn on_render_start(&mut self, now_microseconds: u64) {
        if self.window_started_microseconds.is_none() {
            self.window_started_microseconds = Some(now_microseconds);
        }
        self.current_frame_start = Some(now_microseconds);
    }

    /// Records an invalidated area of `pixels` pixels (`LV_EVENT_INVALIDATE_AREA`).
    ///
    /// `class` names the object the area belongs to. Finding it costs a walk of
    /// the object tree, so it is only called for an area larger than every
    /// other one of the reporting window so far.
    pub fn on_invalidate(&mut self, pixels: u32, class: impl FnOnce() -> Option<&'static str>) {
        self.invalidations = self.invalidations.saturating_add(1);
        self.invalidated_pixels = self.invalidated_pixels.saturating_add(u64::from(pixels));
        if self
            .largest_invalidation
            .is_none_or(|largest| pixels > largest.pixels)
        {
            self.largest_invalidation = Some(Invalidation {
                pixels,
                class: class(),
            });
        }
    }

    /// Records entry into LVGL's flush callback (`LV_EVENT_FLUSH_START`) for an
    /// area of `pixels` pixels.
    pub fn on_flush_start(&mut self, now_microseconds: u64, pixels: u32) {
        self.current_flush_start = Some(now_microseconds);
        self.flushed_pixels = self.flushed_pixels.saturating_add(u64::from(pixels));
    }

    /// Records return from LVGL's flush callback (`LV_EVENT_FLUSH_FINISH`).
    pub fn on_flush_finish(&mut self, now_microseconds: u64) {
        if let Some(started) = self.current_flush_start.take() {
            self.flushes = self.flushes.saturating_add(1);
            self.flush_total_microseconds = self
                .flush_total_microseconds
                .saturating_add(now_microseconds.saturating_sub(started));
        }
    }

    /// Records entry into LVGL's flush wait callback (`LV_EVENT_FLUSH_WAIT_START`).
    pub fn on_wait_start(&mut self, now_microseconds: u64) {
        self.current_wait_start = Some(now_microseconds);
    }

    /// Records return from LVGL's flush wait callback (`LV_EVENT_FLUSH_WAIT_FINISH`).
    pub fn on_wait_finish(&mut self, now_microseconds: u64) {
        if let Some(started) = self.current_wait_start.take() {
            self.wait_total_microseconds = self
                .wait_total_microseconds
                .saturating_add(now_microseconds.saturating_sub(started));
        }
    }

    /// Records completion of an LVGL frame (`LV_EVENT_RENDER_READY`) and returns
    /// a report when the reporting window has elapsed.
    pub fn on_render_ready(&mut self, now_microseconds: u64) -> Option<DisplayPerformanceReport> {
        if let Some(started) = self.current_frame_start.take() {
            let duration = now_microseconds.saturating_sub(started);
            self.frames = self.frames.saturating_add(1);
            self.frame_total_microseconds = self.frame_total_microseconds.saturating_add(duration);
            if duration > self.frame_maximum_microseconds {
                self.frame_maximum_microseconds = duration;
            }
        }

        let window_start = self.window_started_microseconds?;
        let elapsed = now_microseconds.saturating_sub(window_start);
        if self.frames == 0 || elapsed < self.report_interval_microseconds {
            return None;
        }

        let frame_count = u64::from(self.frames);
        let report = DisplayPerformanceReport {
            elapsed_microseconds: elapsed,
            frames: self.frames,
            flushes: self.flushes,
            frame_average_microseconds: self.frame_total_microseconds / frame_count,
            frame_maximum_microseconds: self.frame_maximum_microseconds,
            flush_average_microseconds: self.flush_total_microseconds / frame_count,
            wait_average_microseconds: self.wait_total_microseconds / frame_count,
            invalidations_average_tenths: u64::from(self.invalidations) * 10 / frame_count,
            invalidated_pixels_average: self.invalidated_pixels / frame_count,
            flushed_pixels_average: self.flushed_pixels / frame_count,
            largest_invalidation: self.largest_invalidation,
        };

        self.window_started_microseconds = Some(now_microseconds);
        self.frames = 0;
        self.flushes = 0;
        self.frame_total_microseconds = 0;
        self.frame_maximum_microseconds = 0;
        self.flush_total_microseconds = 0;
        self.wait_total_microseconds = 0;
        self.invalidations = 0;
        self.invalidated_pixels = 0;
        self.flushed_pixels = 0;
        self.largest_invalidation = None;
        Some(report)
    }
}
