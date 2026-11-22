//! Log admission policy for a console that also carries an interactive session.
//!
//! Before a session starts, every enabled message reaches the console: boot
//! messages are the only way to follow the startup. Once a person may be typing
//! on the same port, informational messages are left out, and warnings and
//! errors are limited to a small burst, so a stream of messages cannot bury the
//! session. Messages dropped by the limit, or by a writer that had no room for
//! them, are counted and reported with the next message that passes.

use core::time::Duration;

/// Warnings and errors that may reach an interactive console in one burst.
pub const INTERACTIVE_BURST: u8 = 4;

/// Time after which an interactive console accepts one more warning or error.
pub const INTERACTIVE_REFILL: Duration = Duration::from_secs(1);

/// How important a message is to the person at the console.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LogImportance {
    /// Progress and status messages.
    Information,
    /// Warnings and errors.
    Problem,
}

/// Whether a message may be written, and what to tell the reader first.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LogAdmission {
    /// Leave the message out.
    Drop,
    /// Write the message, after a note that this many earlier messages were
    /// dropped by the rate limit (none when zero).
    Write { dropped: u32 },
}

/// Decides which log messages reach a console. See the module documentation.
#[derive(Debug)]
pub struct LogGate {
    interactive: bool,
    tokens: u8,
    refilled_at: Duration,
    dropped: u32,
}

impl Default for LogGate {
    fn default() -> Self {
        Self::new()
    }
}

impl LogGate {
    /// A gate for a console without an interactive session yet.
    pub const fn new() -> Self {
        Self {
            interactive: false,
            tokens: INTERACTIVE_BURST,
            refilled_at: Duration::ZERO,
            dropped: 0,
        }
    }

    /// Whether an interactive session uses the console.
    pub const fn is_interactive(&self) -> bool {
        self.interactive
    }

    /// Starts or ends the interactive policy. Starting it grants a full burst
    /// from `now`.
    pub fn set_interactive(&mut self, interactive: bool, now: Duration) {
        if interactive && !self.interactive {
            self.tokens = INTERACTIVE_BURST;
            self.refilled_at = now;
        }
        self.interactive = interactive;
    }

    /// Decides whether a message of `importance`, emitted at `now` on a
    /// monotonic clock, may be written.
    pub fn admit(&mut self, importance: LogImportance, now: Duration) -> LogAdmission {
        if !self.interactive {
            return LogAdmission::Write {
                dropped: core::mem::take(&mut self.dropped),
            };
        }
        if importance == LogImportance::Information {
            return LogAdmission::Drop;
        }

        self.refill(now);
        if self.tokens == 0 {
            self.dropped = self.dropped.saturating_add(1);
            return LogAdmission::Drop;
        }
        self.tokens -= 1;
        LogAdmission::Write {
            dropped: core::mem::take(&mut self.dropped),
        }
    }

    /// Counts `count` messages that were admitted but could not be written,
    /// for example because the console queue was full. The next message that
    /// passes reports them.
    pub fn record_dropped(&mut self, count: u32) {
        self.dropped = self.dropped.saturating_add(count);
    }

    fn refill(&mut self, now: Duration) {
        // A clock that went backwards restarts the refill period.
        let Some(elapsed) = now.checked_sub(self.refilled_at) else {
            self.refilled_at = now;
            return;
        };
        let periods = elapsed.as_nanos() / INTERACTIVE_REFILL.as_nanos();
        if periods == 0 {
            return;
        }
        let missing = INTERACTIVE_BURST - self.tokens;
        if periods >= u128::from(missing) {
            self.tokens = INTERACTIVE_BURST;
            self.refilled_at = now;
        } else {
            // `periods` is below `missing`, which fits in a `u8`.
            self.tokens += periods as u8;
            self.refilled_at += INTERACTIVE_REFILL * periods as u32;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const fn seconds(value: u64) -> Duration {
        Duration::from_secs(value)
    }

    #[test]
    fn boot_messages_all_pass() {
        let mut gate = LogGate::new();
        for _ in 0..100 {
            assert_eq!(
                gate.admit(LogImportance::Information, seconds(0)),
                LogAdmission::Write { dropped: 0 }
            );
            assert_eq!(
                gate.admit(LogImportance::Problem, seconds(0)),
                LogAdmission::Write { dropped: 0 }
            );
        }
    }

    #[test]
    fn interactive_drops_information() {
        let mut gate = LogGate::new();
        gate.set_interactive(true, seconds(10));
        assert_eq!(
            gate.admit(LogImportance::Information, seconds(10)),
            LogAdmission::Drop
        );
        // Information is routed away, not rate limited: it is never reported.
        assert_eq!(
            gate.admit(LogImportance::Problem, seconds(10)),
            LogAdmission::Write { dropped: 0 }
        );
    }

    #[test]
    fn interactive_limits_problems_and_reports_the_dropped_count() {
        let mut gate = LogGate::new();
        gate.set_interactive(true, seconds(10));
        for _ in 0..INTERACTIVE_BURST {
            assert_eq!(
                gate.admit(LogImportance::Problem, seconds(10)),
                LogAdmission::Write { dropped: 0 }
            );
        }
        for _ in 0..3 {
            assert_eq!(
                gate.admit(LogImportance::Problem, seconds(10)),
                LogAdmission::Drop
            );
        }
        assert_eq!(
            gate.admit(LogImportance::Problem, seconds(11)),
            LogAdmission::Write { dropped: 3 }
        );
        assert_eq!(
            gate.admit(LogImportance::Problem, seconds(11)),
            LogAdmission::Drop
        );
    }

    #[test]
    fn refill_is_capped_at_one_burst() {
        let mut gate = LogGate::new();
        gate.set_interactive(true, seconds(0));
        for _ in 0..INTERACTIVE_BURST {
            gate.admit(LogImportance::Problem, seconds(0));
        }
        let later = seconds(1000);
        for _ in 0..INTERACTIVE_BURST {
            assert!(matches!(
                gate.admit(LogImportance::Problem, later),
                LogAdmission::Write { .. }
            ));
        }
        assert_eq!(
            gate.admit(LogImportance::Problem, later),
            LogAdmission::Drop
        );
    }

    #[test]
    fn partial_refill_keeps_the_remainder() {
        let mut gate = LogGate::new();
        gate.set_interactive(true, seconds(0));
        for _ in 0..INTERACTIVE_BURST {
            gate.admit(LogImportance::Problem, seconds(0));
        }
        // 1.5 periods: one token, and the half period counts towards the next.
        let now = Duration::from_millis(1500);
        assert!(matches!(
            gate.admit(LogImportance::Problem, now),
            LogAdmission::Write { .. }
        ));
        assert_eq!(gate.admit(LogImportance::Problem, now), LogAdmission::Drop);
        assert!(matches!(
            gate.admit(LogImportance::Problem, seconds(2)),
            LogAdmission::Write { .. }
        ));
    }

    #[test]
    fn leaving_interactive_mode_reports_dropped_messages() {
        let mut gate = LogGate::new();
        gate.set_interactive(true, seconds(0));
        for _ in 0..INTERACTIVE_BURST + 2 {
            gate.admit(LogImportance::Problem, seconds(0));
        }
        gate.set_interactive(false, seconds(0));
        assert_eq!(
            gate.admit(LogImportance::Information, seconds(0)),
            LogAdmission::Write { dropped: 2 }
        );
    }

    #[test]
    fn messages_dropped_by_the_writer_are_reported_next() {
        let mut gate = LogGate::new();
        assert_eq!(
            gate.admit(LogImportance::Information, seconds(0)),
            LogAdmission::Write { dropped: 0 }
        );
        gate.record_dropped(1);
        gate.record_dropped(2);
        assert_eq!(
            gate.admit(LogImportance::Information, seconds(0)),
            LogAdmission::Write { dropped: 3 }
        );
        assert_eq!(
            gate.admit(LogImportance::Information, seconds(0)),
            LogAdmission::Write { dropped: 0 }
        );
    }

    #[test]
    fn clock_going_backwards_does_not_refill() {
        let mut gate = LogGate::new();
        gate.set_interactive(true, seconds(10));
        for _ in 0..INTERACTIVE_BURST {
            gate.admit(LogImportance::Problem, seconds(10));
        }
        assert_eq!(
            gate.admit(LogImportance::Problem, seconds(5)),
            LogAdmission::Drop
        );
        assert_eq!(
            gate.admit(LogImportance::Problem, seconds(5)),
            LogAdmission::Drop
        );
        assert!(matches!(
            gate.admit(LogImportance::Problem, seconds(6)),
            LogAdmission::Write { dropped: 2 }
        ));
    }
}
