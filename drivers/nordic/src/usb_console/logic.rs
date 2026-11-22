use core::fmt::Write;

/// Whether the host asked for the Arduino "1200 bps touch": the port set to
/// 1200 baud, then closed.
///
/// Evaluated on the line state as it stands after a change rather than on the
/// order of the changes: the host may set the rate and drop DTR before the
/// device looks, and a touch that goes unseen leaves the board unreachable
/// without its reset button.
pub const fn is_1200_bps_touch(baud_rate: u32, dtr: bool) -> bool {
    baud_rate == 1200 && !dtr
}

pub use drivers_shared::ring_buffer::RingBuffer;

/// The console overflow policy: new bytes are always accepted and the oldest
/// bytes are dropped to make room for them.
///
/// The terminal keeps the opposite policy (reject what does not fit) on the same
/// [`RingBuffer`] storage, so the policy lives with each driver.
pub trait OverwriteOldest {
    /// Push `data`, dropping the oldest bytes when the ring is full.
    ///
    /// If `data.len() >= N`, the ring is replaced with the last `N` bytes of `data`.
    /// Otherwise, if `len + data.len() > N`, the oldest `len + data.len() - N` bytes are dropped.
    /// Always returns `data.len()`.
    fn push_slice_overwrite(&mut self, data: &[u8]) -> usize;
}

impl<const N: usize> OverwriteOldest for RingBuffer<N> {
    fn push_slice_overwrite(&mut self, data: &[u8]) -> usize {
        if data.is_empty() {
            return 0;
        }

        if data.len() >= N {
            self.replace_with_tail(data);
            return data.len();
        }

        let available_space = self.available_space();
        if data.len() > available_space {
            self.drop_oldest(data.len() - available_space);
        }

        // The space was made above, so every byte fits.
        self.push_slice(data)
    }
}

/// Helper adapter to format records directly into a ring buffer.
pub struct QueueWriter<'a, const N: usize>(pub &'a mut RingBuffer<N>);

impl<const N: usize> Write for QueueWriter<'_, N> {
    fn write_str(&mut self, s: &str) -> core::fmt::Result {
        self.0.push_slice_overwrite(s.as_bytes());
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn drain<const N: usize>(ring: &mut RingBuffer<N>) -> std::vec::Vec<u8> {
        let mut out = [0u8; N];
        let count = ring.pop_slice(&mut out);
        out[..count].to_vec()
    }

    #[test]
    fn test_ring_buffer_nothing_dropped_below_capacity() {
        let mut rb: RingBuffer<8> = RingBuffer::new();
        assert_eq!(rb.push_slice_overwrite(b"123"), 3);
        assert_eq!(rb.push_slice_overwrite(b"45"), 2);
        assert_eq!(rb.push_slice_overwrite(b"678"), 3);
        assert_eq!(rb.len(), 8);
        assert!(rb.is_full());
        assert_eq!(drain(&mut rb), b"12345678");
    }

    #[test]
    fn test_ring_buffer_drop_policy_when_full() {
        let mut rb: RingBuffer<6> = RingBuffer::new();
        // Fill to capacity
        rb.push_slice_overwrite(b"abcdef");
        assert_eq!(rb.len(), 6);

        // Writing 2 bytes drops the 2 oldest bytes ('a', 'b')
        assert_eq!(rb.push_slice_overwrite(b"gh"), 2);
        assert_eq!(rb.len(), 6);
        assert_eq!(drain(&mut rb), b"cdefgh");

        // Overwriting with more than capacity keeps only the tail
        assert_eq!(rb.push_slice_overwrite(b"123456789"), 9);
        assert_eq!(rb.len(), 6);
        assert_eq!(drain(&mut rb), b"456789");
    }

    #[test]
    fn test_ring_buffer_overwrite_exactly_capacity_replaces_everything() {
        let mut rb: RingBuffer<4> = RingBuffer::new();
        rb.push_slice(b"ab");
        assert_eq!(rb.push_slice_overwrite(b"wxyz"), 4);
        assert_eq!(drain(&mut rb), b"wxyz");
    }

    #[test]
    fn test_ring_buffer_overwrite_empty_slice_changes_nothing() {
        let mut rb: RingBuffer<4> = RingBuffer::new();
        rb.push_slice(b"ab");
        assert_eq!(rb.push_slice_overwrite(b""), 0);
        assert_eq!(drain(&mut rb), b"ab");
    }

    #[test]
    fn test_ring_buffer_overwrite_wraps_around() {
        let mut rb: RingBuffer<5> = RingBuffer::new();
        rb.push_slice(b"abcde");
        let mut out = [0u8; 3];
        assert_eq!(rb.pop_slice(&mut out), 3);
        // Two bytes remain and the next three fit without dropping any.
        assert_eq!(rb.push_slice_overwrite(b"fgh"), 3);
        assert_eq!(rb.len(), 5);
        // Four more bytes drop the four oldest ones ('d', 'e', 'f', 'g').
        assert_eq!(rb.push_slice_overwrite(b"ijkl"), 4);
        assert_eq!(drain(&mut rb), b"hijkl");
    }

    /// Connected host with a full queue: the output driver leaves the queue
    /// alone and waits (`is_full`), and a partial write never drops stored bytes.
    #[test]
    fn test_connected_host_full_queue_rejects_instead_of_overwriting() {
        let mut rb: RingBuffer<4> = RingBuffer::new();
        assert_eq!(rb.push_slice(b"abc"), 3);
        // Partial transfer: only one byte fits.
        assert_eq!(rb.push_slice(b"def"), 1);
        assert!(rb.is_full());
        // The driver then waits: nothing is accepted and nothing is dropped.
        assert_eq!(rb.push_slice(b"ghi"), 0);
        assert_eq!(drain(&mut rb), b"abcd");
    }

    /// Disconnected host: the queue never blocks the logger and keeps the newest bytes.
    #[test]
    fn test_disconnected_host_keeps_the_newest_bytes() {
        let mut rb: RingBuffer<8> = RingBuffer::new();
        for chunk in [b"aaaa", b"bbbb", b"cccc"] {
            assert_eq!(rb.push_slice_overwrite(chunk), 4);
        }
        assert_eq!(drain(&mut rb), b"bbbbcccc");
    }

    #[test]
    fn test_queue_writer_format() {
        let mut rb: RingBuffer<32> = RingBuffer::new();
        {
            let mut writer = QueueWriter(&mut rb);
            write!(writer, "Count: {} - ok", 42).unwrap();
        }

        let mut buf = [0u8; 32];
        let n = rb.pop_slice(&mut buf);
        assert_eq!(&buf[..n], b"Count: 42 - ok");
    }

    #[test]
    fn test_1200_bps_touch() {
        // The port set to 1200 baud and closed, in whichever order it is seen.
        assert!(is_1200_bps_touch(1200, false));
        // Still open: the host has not closed the port yet.
        assert!(!is_1200_bps_touch(1200, true));
        // Any other rate, open or closed, including the class default.
        assert!(!is_1200_bps_touch(115_200, false));
        assert!(!is_1200_bps_touch(115_200, true));
        assert!(!is_1200_bps_touch(8_000, false));
    }
}
