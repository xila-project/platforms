//! Where an atomic value sits in memory, and what an operation does to it.
//!
//! This part has no hardware access, so the host tests cover it. Every
//! function is inlined into its caller, which runs from RAM.

use core::ops::Range;

/// The addresses that the ESP32 maps to external PSRAM (`SOC_EXTRAM_DATA_LOW`
/// to `SOC_EXTRAM_DATA_HIGH` in ESP-IDF): 4 MiB behind the flash cache.
pub const EXTERNAL_RAM: Range<usize> = 0x3F80_0000..0x3FC0_0000;

/// Whether `address` is in external PSRAM, where the compare-and-set
/// instruction does not work.
#[inline(always)]
pub const fn is_external(address: usize) -> bool {
    address >= EXTERNAL_RAM.start && address < EXTERNAL_RAM.end
}

/// An atomic value of 1, 2 or 4 bytes inside the aligned 32-bit word that
/// holds it. The compare-and-set instruction only works on whole words, so a
/// smaller value is updated by replacing its word and keeping the other bytes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Field {
    /// Address of the aligned word.
    pub word: usize,
    /// Position of the value in the word, in bits (the chip is little-endian).
    shift: u32,
    /// The bits of the value, before the shift.
    mask: u32,
}

impl Field {
    /// The field of `size` bytes at `address`, which must be aligned to
    /// `size`, as every atomic type is.
    #[inline(always)]
    pub const fn locate(address: usize, size: usize) -> Self {
        assert!(size == 1 || size == 2 || size == 4);
        assert!(address.is_multiple_of(size));
        Self {
            word: address & !3,
            shift: (address & 3) as u32 * 8,
            mask: if size == 4 {
                u32::MAX
            } else {
                (1 << (size * 8)) - 1
            },
        }
    }

    /// The value of the field in `word`.
    #[inline(always)]
    pub const fn extract(&self, word: u32) -> u32 {
        (word >> self.shift) & self.mask
    }

    /// `word` with the field set to `value`; bits of `value` outside the
    /// field are dropped.
    #[inline(always)]
    pub const fn insert(&self, word: u32, value: u32) -> u32 {
        (word & !(self.mask << self.shift)) | ((value & self.mask) << self.shift)
    }

    /// `value` cut to the size of the field.
    #[inline(always)]
    pub const fn truncate(&self, value: u32) -> u32 {
        value & self.mask
    }
}

/// What a read-modify-write operation stores.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Operation {
    Exchange,
    Add,
    Subtract,
    And,
    Or,
    Xor,
    Nand,
}

impl Operation {
    /// The value to store, given the `old` value and the `operand`. The
    /// caller cuts it to the size of the field.
    #[inline(always)]
    pub const fn apply(self, old: u32, operand: u32) -> u32 {
        match self {
            Self::Exchange => operand,
            Self::Add => old.wrapping_add(operand),
            Self::Subtract => old.wrapping_sub(operand),
            Self::And => old & operand,
            Self::Or => old | operand,
            Self::Xor => old ^ operand,
            Self::Nand => !(old & operand),
        }
    }
}

/// Replaces the value of `field` by `update(old)` with a word compare-and-set,
/// and returns the old value.
///
/// `load` reads the word. `compare_and_set(expected, new)` stores `new` if the
/// word still holds `expected`, and returns what the word held. The update is
/// retried until no other writer changed the word in between, so the other
/// bytes of the word are never overwritten with a stale value.
#[inline(always)]
pub fn update(
    field: Field,
    mut load: impl FnMut() -> u32,
    mut compare_and_set: impl FnMut(u32, u32) -> u32,
    update: impl Fn(u32) -> u32,
) -> u32 {
    loop {
        let current = load();
        let old = field.extract(current);
        if compare_and_set(current, field.insert(current, update(old))) == current {
            return old;
        }
    }
}

/// Stores `new` in `field` if it holds `expected`, with a word
/// compare-and-set, and returns the value the field held. A change to another
/// byte of the word is retried, not reported as a failure.
#[inline(always)]
pub fn compare_exchange(
    field: Field,
    mut load: impl FnMut() -> u32,
    mut compare_and_set: impl FnMut(u32, u32) -> u32,
    expected: u32,
    new: u32,
) -> u32 {
    let expected = field.truncate(expected);
    loop {
        let current = load();
        let old = field.extract(current);
        if old != expected || compare_and_set(current, field.insert(current, new)) == current {
            return old;
        }
    }
}
