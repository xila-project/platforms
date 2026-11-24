//! When to start the application again after it ends.

use core::time::Duration;

/// Restarts the application at once after a long run, and with a growing
/// delay after short ones, so that an application that fails at start does
/// not keep the processor busy.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Restart {
    /// Short runs in a row.
    short_runs: u32,
}

impl Restart {
    /// A run at least this long counts as a normal run: the next start is
    /// immediate and the delay starts over.
    pub const STABLE_RUN: Duration = Duration::from_secs(10);
    /// The delay after the first short run, doubled after each further one.
    pub const FIRST_DELAY: Duration = Duration::from_secs(1);
    /// The longest delay.
    pub const MAXIMUM_DELAY: Duration = Duration::from_secs(60);

    pub const fn new() -> Self {
        Self { short_runs: 0 }
    }

    /// How long to wait before the next start, after a run of `duration`.
    pub fn delay_after(&mut self, duration: Duration) -> Duration {
        if duration >= Self::STABLE_RUN {
            self.short_runs = 0;
            return Duration::ZERO;
        }
        // 1, 2, 4 ... 32 seconds, then the maximum.
        let delay = Self::FIRST_DELAY
            .saturating_mul(1 << self.short_runs.min(6))
            .min(Self::MAXIMUM_DELAY);
        self.short_runs = self.short_runs.saturating_add(1);
        delay
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn seconds(value: u64) -> Duration {
        Duration::from_secs(value)
    }

    #[test]
    fn short_runs_back_off_up_to_the_maximum() {
        let mut restart = Restart::new();
        let delays: [u64; 9] = core::array::from_fn(|_| restart.delay_after(seconds(0)).as_secs());
        assert_eq!(delays, [1, 2, 4, 8, 16, 32, 60, 60, 60]);
    }

    #[test]
    fn a_long_run_restarts_at_once_and_resets_the_delay() {
        let mut restart = Restart::new();
        assert_eq!(restart.delay_after(seconds(3)), seconds(1));
        assert_eq!(restart.delay_after(seconds(3)), seconds(2));
        assert_eq!(restart.delay_after(Restart::STABLE_RUN), Duration::ZERO);
        assert_eq!(restart.delay_after(seconds(3600)), Duration::ZERO);
        assert_eq!(restart.delay_after(seconds(9)), seconds(1));
    }
}
