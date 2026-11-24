//! Lightweight display render and flush performance tracker.
//!
//! Accumulates per-frame render duration, flush callback time, and asynchronous
//! flush wait time over a reporting window so firmware can emit a single
//! summary line over the serial console when debug logging is enabled.

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
        formatter.write_str(" ms)")
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

    /// Records entry into LVGL's flush callback (`LV_EVENT_FLUSH_START`).
    pub fn on_flush_start(&mut self, now_microseconds: u64) {
        self.current_flush_start = Some(now_microseconds);
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
        };

        self.window_started_microseconds = Some(now_microseconds);
        self.frames = 0;
        self.flushes = 0;
        self.frame_total_microseconds = 0;
        self.frame_maximum_microseconds = 0;
        self.flush_total_microseconds = 0;
        self.wait_total_microseconds = 0;
        Some(report)
    }
}
