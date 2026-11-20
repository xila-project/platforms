//! Bounded pointer event queue shared by the desktop and browser pointer drivers.
//!
//! LVGL samples a pointer device on a fixed timer, so a press and its release that both arrive
//! between two reads must reach it as two successive samples; keeping only the latest state
//! loses the click. The queue stores complete samples (position and button state). Producers push
//! the state they observe; the reader pops one sample per call and learns whether more are
//! pending, which maps to the `continue` flag of the Core input data.
//!
//! The queue is bounded, never drops a button transition for a move and always ends in the
//! physical button state:
//!
//! - consecutive moves coalesce into the newest queued move, so a mouse-move flood never grows it;
//! - a move that arrives while the queue is full is dropped (the reader falls back to the latest
//!   position once the queue is drained);
//! - a press or release evicts the oldest queued move to make room;
//! - when the queue holds only transitions (the reader stalled for 16 clicks with the default
//!   capacity), the newest queued transition is removed and the incoming one takes its place. The
//!   transitions alternate, so exactly one complete click is lost and the final state is the one
//!   the user left the button in. Window and touch events are never re-sent, so refusing the
//!   incoming transition instead could leave a button stuck.

use core::cell::RefCell;

use critical_section::Mutex;

/// Default capacity: enough for several clicks within one 33 ms LVGL frame.
pub const POINTER_QUEUE_CAPACITY: usize = 32;

/// One pointer sample: a position and whether the primary button is held.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct PointerSample {
    pub x: i16,
    pub y: i16,
    pub pressed: bool,
}

#[derive(Clone, Copy, Debug, Default)]
struct Entry {
    sample: PointerSample,
    /// `true` for a position change without a button transition (the only coalescable kind).
    is_move: bool,
}

/// Bounded queue of pointer samples with move coalescing.
#[derive(Debug)]
pub struct PointerQueue<const N: usize> {
    entries: [Entry; N],
    head: usize,
    len: usize,
    /// Latest state observed by producers. Its button state always matches the newest queued
    /// entry, and it is what the reader reports once the queue is drained.
    latest: PointerSample,
}

impl<const N: usize> Default for PointerQueue<N> {
    fn default() -> Self {
        Self::new()
    }
}

impl<const N: usize> PointerQueue<N> {
    pub const fn new() -> Self {
        const {
            assert!(N >= 2, "a press and its release must fit in the queue");
        }
        Self {
            entries: [Entry {
                sample: PointerSample {
                    x: 0,
                    y: 0,
                    pressed: false,
                },
                is_move: false,
            }; N],
            head: 0,
            len: 0,
            latest: PointerSample {
                x: 0,
                y: 0,
                pressed: false,
            },
        }
    }

    pub const fn len(&self) -> usize {
        self.len
    }

    pub const fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// Latest state observed by producers (what the pointer looks like once the queue is drained).
    pub const fn latest(&self) -> PointerSample {
        self.latest
    }

    fn slot(&self, offset: usize) -> usize {
        (self.head + offset) % N
    }

    /// Records the observed pointer state, queueing a sample when something changed.
    ///
    /// A button transition is queued with its position and is never merged or dropped for a move.
    /// A position change on its own is merged into the newest queued move.
    pub fn update(&mut self, x: i16, y: i16, pressed: bool) {
        let sample = PointerSample { x, y, pressed };
        if sample == self.latest {
            return;
        }

        if pressed != self.latest.pressed {
            if self.len == N && !self.evict_oldest_move() {
                // Only alternating transitions are queued, so the newest one has the opposite
                // state of the incoming one and the entry before it (`N >= 2`) has the same
                // state. Dropping the newest and refreshing that entry loses one complete click
                // and keeps the queue ending in the state of `latest`.
                self.len -= 1;
                let newest = self.slot(self.len - 1);
                self.entries[newest].sample = sample;
                self.latest = sample;
                return;
            }
            self.latest = sample;
            self.push_back(Entry {
                sample,
                is_move: false,
            });
            return;
        }

        self.latest = sample;
        if self.len > 0 {
            let newest = self.slot(self.len - 1);
            if self.entries[newest].is_move {
                self.entries[newest].sample = sample;
                return;
            }
        }
        if self.len < N {
            self.push_back(Entry {
                sample,
                is_move: true,
            });
        }
        // Full of transitions: drop the move. `latest` carries the position once drained.
    }

    /// Moves the pointer, keeping the button state.
    pub fn move_to(&mut self, x: i16, y: i16) {
        self.update(x, y, self.latest.pressed);
    }

    /// Presses or releases the button at the current position.
    pub fn set_pressed(&mut self, pressed: bool) {
        self.update(self.latest.x, self.latest.y, pressed);
    }

    fn push_back(&mut self, entry: Entry) {
        let slot = self.slot(self.len);
        self.entries[slot] = entry;
        self.len += 1;
    }

    /// Removes the oldest queued move, preserving the order of the other entries.
    fn evict_oldest_move(&mut self) -> bool {
        let Some(position) = (0..self.len).find(|&offset| self.entries[self.slot(offset)].is_move)
        else {
            return false;
        };
        for offset in position..self.len - 1 {
            let (to, from) = (self.slot(offset), self.slot(offset + 1));
            self.entries[to] = self.entries[from];
        }
        self.len -= 1;
        true
    }

    /// Takes the next sample for the reader.
    ///
    /// Returns the sample to report and whether more samples are queued. With nothing queued, the
    /// latest observed state is returned, so a held button stays held and a position change that
    /// could not be queued still reaches the reader.
    pub fn next_sample(&mut self) -> (PointerSample, bool) {
        if self.len == 0 {
            return (self.latest, false);
        }
        let sample = self.entries[self.head].sample;
        self.head = (self.head + 1) % N;
        self.len -= 1;
        (sample, self.len > 0)
    }
}

/// [`PointerQueue`] behind a critical section, shareable between event producers and the reader.
#[derive(Debug)]
pub struct SharedPointerQueue<const N: usize = POINTER_QUEUE_CAPACITY>(
    Mutex<RefCell<PointerQueue<N>>>,
);

impl<const N: usize> Default for SharedPointerQueue<N> {
    fn default() -> Self {
        Self::new()
    }
}

impl<const N: usize> SharedPointerQueue<N> {
    pub const fn new() -> Self {
        Self(Mutex::new(RefCell::new(PointerQueue::new())))
    }

    fn with<R>(&self, operation: impl FnOnce(&mut PointerQueue<N>) -> R) -> R {
        critical_section::with(|cs| operation(&mut self.0.borrow(cs).borrow_mut()))
    }

    pub fn update(&self, x: i16, y: i16, pressed: bool) {
        self.with(|queue| queue.update(x, y, pressed));
    }

    pub fn move_to(&self, x: i16, y: i16) {
        self.with(|queue| queue.move_to(x, y));
    }

    pub fn set_pressed(&self, pressed: bool) {
        self.with(|queue| queue.set_pressed(pressed));
    }

    pub fn next_sample(&self) -> (PointerSample, bool) {
        self.with(PointerQueue::next_sample)
    }

    pub fn latest(&self) -> PointerSample {
        self.with(|queue| queue.latest())
    }
}

#[cfg(test)]
mod tests {
    use std::vec::Vec;

    use super::*;

    fn sample(x: i16, y: i16, pressed: bool) -> PointerSample {
        PointerSample { x, y, pressed }
    }

    #[test]
    fn press_and_release_between_reads_are_both_delivered() {
        let mut queue = PointerQueue::<8>::new();
        queue.update(10, 20, true);
        queue.update(10, 20, false);

        assert_eq!(queue.next_sample(), (sample(10, 20, true), true));
        assert_eq!(queue.next_sample(), (sample(10, 20, false), false));
    }

    #[test]
    fn empty_queue_keeps_reporting_the_held_state() {
        let mut queue = PointerQueue::<8>::new();
        queue.update(3, 4, true);
        assert_eq!(queue.next_sample(), (sample(3, 4, true), false));
        assert_eq!(queue.next_sample(), (sample(3, 4, true), false));
        assert_eq!(queue.next_sample(), (sample(3, 4, true), false));
    }

    #[test]
    fn consecutive_moves_coalesce_into_one_sample() {
        let mut queue = PointerQueue::<8>::new();
        for x in 1..=1000 {
            queue.move_to(x, 2 * x);
        }
        assert_eq!(queue.len(), 1);
        assert_eq!(queue.next_sample(), (sample(1000, 2000, false), false));
    }

    #[test]
    fn a_move_never_merges_into_a_button_transition() {
        let mut queue = PointerQueue::<8>::new();
        queue.update(5, 5, true);
        queue.move_to(50, 50);
        queue.move_to(60, 60);
        queue.set_pressed(false);

        assert_eq!(queue.next_sample(), (sample(5, 5, true), true));
        assert_eq!(queue.next_sample(), (sample(60, 60, true), true));
        assert_eq!(queue.next_sample(), (sample(60, 60, false), false));
    }

    #[test]
    fn repeated_identical_samples_queue_nothing() {
        let mut queue = PointerQueue::<8>::new();
        queue.update(1, 1, false);
        queue.update(1, 1, false);
        assert_eq!(queue.len(), 1);
        queue.next_sample();
        queue.update(1, 1, false);
        assert!(queue.is_empty());
    }

    #[test]
    fn button_transition_evicts_the_oldest_move_when_full() {
        let mut queue = PointerQueue::<4>::new();
        // press, release, press: three transitions, then a move fills the queue.
        queue.update(0, 0, true);
        queue.update(0, 0, false);
        queue.update(0, 0, true);
        queue.move_to(9, 9);
        assert_eq!(queue.len(), 4);

        queue.set_pressed(false);
        assert_eq!(queue.len(), 4);
        let mut drained = [PointerSample::default(); 4];
        for slot in &mut drained {
            *slot = queue.next_sample().0;
        }
        assert_eq!(
            drained,
            [
                sample(0, 0, true),
                sample(0, 0, false),
                sample(0, 0, true),
                sample(9, 9, false),
            ]
        );
        assert!(queue.is_empty());
    }

    #[test]
    fn move_is_dropped_when_full_but_position_reaches_the_next_transition() {
        let mut queue = PointerQueue::<2>::new();
        queue.update(0, 0, true);
        queue.update(0, 0, false);
        assert_eq!(queue.len(), 2);

        queue.move_to(7, 8);
        assert_eq!(queue.len(), 2);
        assert_eq!(queue.latest(), sample(7, 8, false));

        assert_eq!(queue.next_sample(), (sample(0, 0, true), true));
        assert_eq!(queue.next_sample(), (sample(0, 0, false), false));
        queue.set_pressed(true);
        assert_eq!(queue.next_sample(), (sample(7, 8, true), false));
    }

    /// Drains the queue, returning every reported sample.
    fn drain<const N: usize>(queue: &mut PointerQueue<N>) -> Vec<PointerSample> {
        let mut samples = Vec::new();
        loop {
            let (sample, more) = queue.next_sample();
            samples.push(sample);
            if !more {
                return samples;
            }
        }
    }

    #[test]
    fn full_queue_of_transitions_drops_the_newest_click_but_keeps_the_final_state() {
        let mut queue = PointerQueue::<2>::new();
        queue.update(1, 1, true);
        queue.update(1, 1, false);

        // A new press replaces the queued release: one click is lost, the button ends pressed.
        queue.update(5, 6, true);
        assert_eq!(queue.len(), 1);
        assert_eq!(queue.latest(), sample(5, 6, true));
        // The release that follows has room again and is delivered.
        queue.update(5, 6, false);
        assert_eq!(drain(&mut queue), [sample(5, 6, true), sample(5, 6, false)]);
    }

    #[test]
    fn release_is_never_lost_while_the_reader_stalls() {
        let mut queue = PointerQueue::<POINTER_QUEUE_CAPACITY>::new();
        // 20 complete clicks (more than capacity / 2) while LVGL does not read.
        for click in 0..20i16 {
            queue.update(click, click, true);
            queue.update(click, click, false);
        }
        assert!(queue.len() <= POINTER_QUEUE_CAPACITY);
        assert!(!queue.latest().pressed);

        let samples = drain(&mut queue);
        assert_eq!(samples.len(), POINTER_QUEUE_CAPACITY);
        // Transitions alternate, starting with a press and ending with a release.
        for (index, sample) in samples.iter().enumerate() {
            assert_eq!(sample.pressed, index % 2 == 0, "sample {index}");
        }
        assert!(!samples.last().unwrap().pressed);
        // Once drained, the queue keeps reporting a released button.
        assert_eq!(queue.next_sample(), (sample(19, 19, false), false));
        assert_eq!(queue.next_sample(), (sample(19, 19, false), false));
    }

    #[test]
    fn release_is_never_lost_when_the_queue_is_full_while_held() {
        let mut queue = PointerQueue::<POINTER_QUEUE_CAPACITY>::new();
        // 16 clicks fill the queue, then the 17th press is held and finally released.
        for click in 0..16i16 {
            queue.update(click, 0, true);
            queue.update(click, 0, false);
        }
        assert_eq!(queue.len(), POINTER_QUEUE_CAPACITY);
        queue.update(40, 40, true);
        assert_eq!(queue.latest(), sample(40, 40, true));
        queue.update(41, 41, false);

        let samples = drain(&mut queue);
        assert_eq!(samples.len(), POINTER_QUEUE_CAPACITY);
        assert!(!samples.last().unwrap().pressed);
        assert_eq!(queue.next_sample(), (sample(41, 41, false), false));
        assert_eq!(queue.latest(), sample(41, 41, false));
    }

    #[test]
    fn dropped_move_still_updates_the_idle_position() {
        let mut queue = PointerQueue::<2>::new();
        queue.update(0, 0, true);
        queue.update(0, 0, false);
        // Dropped: the queue is full of transitions, but `latest` records the position.
        queue.move_to(7, 8);

        assert_eq!(drain(&mut queue), [sample(0, 0, true), sample(0, 0, false)]);
        // The reader now sees the position of the dropped move, not the stale (0, 0).
        assert_eq!(queue.next_sample(), (sample(7, 8, false), false));
        // The same position again is not a change, and stays reported.
        queue.move_to(7, 8);
        assert!(queue.is_empty());
        assert_eq!(queue.next_sample(), (sample(7, 8, false), false));
    }

    #[test]
    fn flood_never_loses_a_click() {
        let mut queue = PointerQueue::<8>::new();
        for round in 0..100i16 {
            for step in 0..50 {
                queue.move_to(round, step);
            }
            queue.set_pressed(true);
            for step in 0..50 {
                queue.move_to(round, step);
            }
            queue.set_pressed(false);

            let mut presses = 0;
            let mut releases = 0;
            let mut previous = false;
            loop {
                let (sample, more) = queue.next_sample();
                if sample.pressed && !previous {
                    presses += 1;
                } else if !sample.pressed && previous {
                    releases += 1;
                }
                previous = sample.pressed;
                if !more {
                    break;
                }
            }
            assert_eq!((presses, releases), (1, 1), "round {round}");
        }
    }

    #[test]
    fn queue_wraps_around_without_reordering() {
        let mut queue = PointerQueue::<3>::new();
        for round in 0..20i16 {
            queue.update(round, 0, true);
            queue.update(round, 0, false);
            assert_eq!(queue.next_sample(), (sample(round, 0, true), true));
            assert_eq!(queue.next_sample(), (sample(round, 0, false), false));
        }
    }

    #[test]
    fn shared_queue_round_trip() {
        static QUEUE: SharedPointerQueue<4> = SharedPointerQueue::new();
        QUEUE.update(2, 3, true);
        QUEUE.set_pressed(false);
        assert_eq!(QUEUE.next_sample(), (sample(2, 3, true), true));
        assert_eq!(QUEUE.next_sample(), (sample(2, 3, false), false));
    }
}
