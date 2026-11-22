//! Fixed-capacity byte ring shared by the console and terminal drivers.
//!
//! The ring only provides storage operations: pushing and popping bytes with
//! partial transfers, and dropping the oldest bytes. What to do when the ring
//! is full (reject the new bytes, or drop the oldest ones) is the policy of the
//! caller, which builds it from [`RingBuffer::available_space`],
//! [`RingBuffer::drop_oldest`] and [`RingBuffer::clear`].

/// A fixed-capacity FIFO byte ring with no heap allocation.
#[derive(Debug, Clone, Copy)]
pub struct RingBuffer<const N: usize> {
    storage: [u8; N],
    read_position: usize,
    write_position: usize,
    length: usize,
}

impl<const N: usize> Default for RingBuffer<N> {
    fn default() -> Self {
        Self::new()
    }
}

impl<const N: usize> RingBuffer<N> {
    /// Create an empty ring.
    pub const fn new() -> Self {
        Self {
            storage: [0; N],
            read_position: 0,
            write_position: 0,
            length: 0,
        }
    }

    /// Total capacity in bytes.
    pub const fn capacity(&self) -> usize {
        N
    }

    /// Number of bytes currently stored.
    pub const fn len(&self) -> usize {
        self.length
    }

    /// Whether the ring holds no byte.
    pub const fn is_empty(&self) -> bool {
        self.length == 0
    }

    /// Whether the ring is completely full.
    pub const fn is_full(&self) -> bool {
        self.length == N
    }

    /// Remaining space in bytes.
    pub const fn available_space(&self) -> usize {
        N - self.length
    }

    /// Remove every stored byte.
    pub const fn clear(&mut self) {
        self.read_position = 0;
        self.write_position = 0;
        self.length = 0;
    }

    /// Drop the oldest `count` bytes without returning them.
    ///
    /// Dropping more than the stored length empties the ring.
    pub fn drop_oldest(&mut self, count: usize) {
        let to_drop = count.min(self.length);
        self.read_position = (self.read_position + to_drop) % N;
        self.length -= to_drop;
    }

    /// Push a single byte.
    ///
    /// Returns `true` if the byte was stored and `false` if the ring is full.
    pub fn push_byte(&mut self, byte: u8) -> bool {
        if self.is_full() {
            return false;
        }
        self.storage[self.write_position] = byte;
        self.write_position = (self.write_position + 1) % N;
        self.length += 1;
        true
    }

    /// Push as many bytes of `data` as fit, without dropping any stored byte.
    ///
    /// Returns the number of bytes pushed.
    pub fn push_slice(&mut self, data: &[u8]) -> usize {
        let to_push = data.len().min(self.available_space());
        for &byte in &data[..to_push] {
            self.storage[self.write_position] = byte;
            self.write_position = (self.write_position + 1) % N;
        }
        self.length += to_push;
        to_push
    }

    /// Pop the oldest byte.
    pub fn pop_byte(&mut self) -> Option<u8> {
        if self.is_empty() {
            return None;
        }
        let byte = self.storage[self.read_position];
        self.read_position = (self.read_position + 1) % N;
        self.length -= 1;
        Some(byte)
    }

    /// Pop up to `out.len()` of the oldest bytes into `out`.
    ///
    /// Returns the number of bytes popped.
    pub fn pop_slice(&mut self, out: &mut [u8]) -> usize {
        let to_read = out.len().min(self.length);
        for byte in &mut out[..to_read] {
            *byte = self.storage[self.read_position];
            self.read_position = (self.read_position + 1) % N;
        }
        self.length -= to_read;
        to_read
    }

    /// Replace the content with the last `N` bytes of `data`.
    ///
    /// This is the storage step of an overwrite policy for a payload that is at
    /// least as large as the ring; the caller decides when to use it.
    pub fn replace_with_tail(&mut self, data: &[u8]) {
        debug_assert!(data.len() >= N);
        let start = data.len() - N;
        self.storage.copy_from_slice(&data[start..]);
        self.read_position = 0;
        self.write_position = 0;
        self.length = N;
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
    fn push_and_pop_partial_transfers() {
        let mut ring: RingBuffer<16> = RingBuffer::new();
        assert!(ring.is_empty());
        assert_eq!(ring.capacity(), 16);
        assert_eq!(ring.available_space(), 16);

        assert_eq!(ring.push_slice(b"hello world"), 11);
        assert_eq!(ring.len(), 11);
        assert!(!ring.is_full());

        let mut out = [0u8; 16];
        assert_eq!(ring.pop_slice(&mut out[..5]), 5);
        assert_eq!(&out[..5], b"hello");
        assert_eq!(ring.pop_slice(&mut out), 6);
        assert_eq!(&out[..6], b" world");
        assert!(ring.is_empty());
        assert_eq!(ring.pop_slice(&mut out), 0);
    }

    #[test]
    fn push_slice_stops_when_full() {
        let mut ring: RingBuffer<4> = RingBuffer::new();
        assert_eq!(ring.push_slice(b"abcdef"), 4);
        assert!(ring.is_full());
        assert_eq!(ring.push_slice(b"x"), 0);
        assert!(!ring.push_byte(b'x'));
        assert_eq!(drain(&mut ring), b"abcd");
    }

    #[test]
    fn wraps_around_the_end_of_the_storage() {
        let mut ring: RingBuffer<4> = RingBuffer::new();
        assert_eq!(ring.push_slice(b"abc"), 3);
        let mut out = [0u8; 2];
        assert_eq!(ring.pop_slice(&mut out), 2);
        assert_eq!(&out, b"ab");

        // Writes 'd', 'e' and 'f' over the end of the storage.
        assert_eq!(ring.push_slice(b"def"), 3);
        assert!(ring.is_full());
        assert_eq!(drain(&mut ring), b"cdef");
        assert!(ring.is_empty());
    }

    #[test]
    fn single_byte_operations_wrap() {
        let mut ring: RingBuffer<2> = RingBuffer::new();
        for byte in 0..10u8 {
            assert!(ring.push_byte(byte));
            assert_eq!(ring.pop_byte(), Some(byte));
        }
        assert_eq!(ring.pop_byte(), None);
    }

    #[test]
    fn drop_oldest_is_clamped_and_wraps() {
        let mut ring: RingBuffer<4> = RingBuffer::new();
        ring.push_slice(b"abcd");
        ring.drop_oldest(1);
        assert!(ring.push_byte(b'e'));
        assert_eq!(drain(&mut ring), b"bcde");

        ring.push_slice(b"ab");
        ring.drop_oldest(100);
        assert!(ring.is_empty());
    }

    #[test]
    fn clear_empties_the_ring() {
        let mut ring: RingBuffer<4> = RingBuffer::new();
        ring.push_slice(b"abc");
        ring.drop_oldest(2);
        ring.clear();
        assert!(ring.is_empty());
        assert_eq!(ring.push_slice(b"wxyz"), 4);
        assert_eq!(drain(&mut ring), b"wxyz");
    }

    #[test]
    fn replace_with_tail_keeps_the_last_bytes() {
        let mut ring: RingBuffer<6> = RingBuffer::new();
        ring.push_slice(b"ab");
        ring.replace_with_tail(b"123456789");
        assert!(ring.is_full());
        assert_eq!(drain(&mut ring), b"456789");

        ring.replace_with_tail(b"abcdef");
        assert_eq!(drain(&mut ring), b"abcdef");
    }
}
