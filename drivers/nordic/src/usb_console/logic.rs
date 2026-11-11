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

/// A fixed-capacity ring buffer with support for FIFO push/pop and overwrite on full.
#[derive(Debug)]
pub struct RingBuffer<const N: usize> {
    storage: [u8; N],
    read_pos: usize,
    write_pos: usize,
    len: usize,
}

impl<const N: usize> Default for RingBuffer<N> {
    fn default() -> Self {
        Self::new()
    }
}

impl<const N: usize> RingBuffer<N> {
    pub const fn new() -> Self {
        Self {
            storage: [0; N],
            read_pos: 0,
            write_pos: 0,
            len: 0,
        }
    }

    pub const fn capacity(&self) -> usize {
        N
    }

    pub const fn len(&self) -> usize {
        self.len
    }

    pub const fn is_empty(&self) -> bool {
        self.len == 0
    }

    pub const fn is_full(&self) -> bool {
        self.len == N
    }

    pub const fn available_space(&self) -> usize {
        N - self.len
    }

    /// Pop `count` oldest bytes from the front without returning them.
    pub fn drop_oldest(&mut self, count: usize) {
        let to_drop = count.min(self.len);
        self.read_pos = (self.read_pos + to_drop) % N;
        self.len -= to_drop;
    }

    /// Push as many bytes as can fit without dropping any bytes.
    ///
    /// Returns the number of bytes pushed.
    pub fn push_slice(&mut self, data: &[u8]) -> usize {
        let to_push = data.len().min(self.available_space());
        for &byte in &data[..to_push] {
            self.storage[self.write_pos] = byte;
            self.write_pos = (self.write_pos + 1) % N;
        }
        self.len += to_push;
        to_push
    }

    /// Push bytes with a "drop oldest" policy when full.
    ///
    /// If `data.len() >= N`, the buffer is replaced with the last `N` bytes of `data`.
    /// Otherwise, if `len + data.len() > N`, the oldest `len + data.len() - N` bytes are dropped.
    /// Always returns `data.len()`.
    pub fn push_slice_overwrite(&mut self, data: &[u8]) -> usize {
        if data.is_empty() {
            return 0;
        }

        if data.len() >= N {
            let start = data.len() - N;
            self.storage.copy_from_slice(&data[start..]);
            self.read_pos = 0;
            self.write_pos = 0;
            self.len = N;
            return data.len();
        }

        let needed_space = data.len();
        if needed_space > self.available_space() {
            let to_drop = needed_space - self.available_space();
            self.drop_oldest(to_drop);
        }

        for &byte in data {
            self.storage[self.write_pos] = byte;
            self.write_pos = (self.write_pos + 1) % N;
        }
        self.len += data.len();
        data.len()
    }

    /// Pop up to `out.len()` bytes into `out`.
    ///
    /// Returns the number of bytes popped.
    pub fn pop_slice(&mut self, out: &mut [u8]) -> usize {
        let to_read = out.len().min(self.len);
        for byte in &mut out[..to_read] {
            *byte = self.storage[self.read_pos];
            self.read_pos = (self.read_pos + 1) % N;
        }
        self.len -= to_read;
        to_read
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

    #[test]
    fn test_ring_buffer_push_pop_basic() {
        let mut rb: RingBuffer<16> = RingBuffer::new();
        assert_eq!(rb.len(), 0);
        assert!(rb.is_empty());
        assert_eq!(rb.available_space(), 16);

        let data = b"hello world";
        let written = rb.push_slice(data);
        assert_eq!(written, data.len());
        assert_eq!(rb.len(), data.len());
        assert!(!rb.is_empty());
        assert!(!rb.is_full());

        let mut out = [0u8; 16];
        let read = rb.pop_slice(&mut out[..5]);
        assert_eq!(read, 5);
        assert_eq!(&out[..5], b"hello");
        assert_eq!(rb.len(), 6);

        let read = rb.pop_slice(&mut out);
        assert_eq!(read, 6);
        assert_eq!(&out[..6], b" world");
        assert!(rb.is_empty());
    }

    #[test]
    fn test_ring_buffer_nothing_dropped_below_capacity() {
        let mut rb: RingBuffer<8> = RingBuffer::new();
        assert_eq!(rb.push_slice_overwrite(b"123"), 3);
        assert_eq!(rb.push_slice_overwrite(b"45"), 2);
        assert_eq!(rb.push_slice_overwrite(b"678"), 3);
        assert_eq!(rb.len(), 8);
        assert!(rb.is_full());

        let mut out = [0u8; 8];
        let n = rb.pop_slice(&mut out);
        assert_eq!(n, 8);
        assert_eq!(&out, b"12345678");
    }

    #[test]
    fn test_ring_buffer_drop_policy_when_full() {
        let mut rb: RingBuffer<6> = RingBuffer::new();
        // Fill to capacity
        rb.push_slice_overwrite(b"abcdef");
        assert_eq!(rb.len(), 6);

        // Writing 2 bytes drops the 2 oldest bytes ('a', 'b')
        rb.push_slice_overwrite(b"gh");
        assert_eq!(rb.len(), 6);

        let mut out = [0u8; 6];
        let n = rb.pop_slice(&mut out);
        assert_eq!(n, 6);
        assert_eq!(&out, b"cdefgh");

        // Overwriting with more than capacity keeps only the tail
        rb.push_slice_overwrite(b"123456789");
        assert_eq!(rb.len(), 6);
        let n = rb.pop_slice(&mut out);
        assert_eq!(n, 6);
        assert_eq!(&out, b"456789");
    }

    #[test]
    fn test_ring_buffer_fifo_wrap_around() {
        let mut rb: RingBuffer<4> = RingBuffer::new();
        // Fill 3 bytes
        assert_eq!(rb.push_slice(b"abc"), 3);
        let mut out = [0u8; 2];
        // Read 2 bytes: read_pos = 2, write_pos = 3, len = 1 ('c')
        assert_eq!(rb.pop_slice(&mut out), 2);
        assert_eq!(&out, b"ab");

        // Write 3 bytes: 'd', 'e', 'f' wrapping around to 0, 1, 2
        assert_eq!(rb.push_slice(b"def"), 3);
        assert_eq!(rb.len(), 4);

        let mut full_out = [0u8; 4];
        let n = rb.pop_slice(&mut full_out);
        assert_eq!(n, 4);
        assert_eq!(&full_out, b"cdef");
        assert!(rb.is_empty());
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
