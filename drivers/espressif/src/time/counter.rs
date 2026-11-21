/// Extends the hardware counter across wraps, provided it is sampled at least
/// once per counter period.
pub(super) struct Counter {
    previous: u64,
    elapsed: u64,
    mask: u64,
    frequency: u64,
}

impl Counter {
    pub(super) const fn new(initial: u64, mask: u64, frequency: u64) -> Self {
        Self {
            previous: initial,
            elapsed: 0,
            mask,
            frequency,
        }
    }

    pub(super) fn microseconds(&mut self, current: u64) -> u64 {
        self.elapsed += current.wrapping_sub(self.previous) & self.mask;
        self.previous = current;
        // Convert only after adding the elapsed ticks, keeping fractional
        // microseconds across samples and across the hardware counter wrap.
        let seconds = self.elapsed / self.frequency;
        let remaining = self.elapsed % self.frequency;
        seconds * 1_000_000 + remaining * 1_000_000 / self.frequency
    }
}

pub(super) fn alarm_has_elapsed(start: u64, delay: u64, end: u64) -> bool {
    end.saturating_sub(start) >= delay
}
