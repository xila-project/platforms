#[path = "../src/atomics/word.rs"]
mod word;

use std::cell::Cell;

use word::{EXTERNAL_RAM, Field, Operation, compare_exchange, is_external, update};

/// A word of memory with a compare-and-set that another writer disturbs a
/// given number of times.
struct Memory {
    word: Cell<u32>,
    /// What another writer stores right before the next compare-and-set.
    interference: Cell<Option<u32>>,
    attempts: Cell<usize>,
}

impl Memory {
    fn new(word: u32) -> Self {
        Self {
            word: Cell::new(word),
            interference: Cell::new(None),
            attempts: Cell::new(0),
        }
    }

    fn load(&self) -> u32 {
        self.word.get()
    }

    fn compare_and_set(&self, expected: u32, new: u32) -> u32 {
        self.attempts.set(self.attempts.get() + 1);
        if let Some(word) = self.interference.take() {
            self.word.set(word);
        }
        let old = self.word.get();
        if old == expected {
            self.word.set(new);
        }
        old
    }
}

#[test]
fn external_ram_is_the_esp32_psram_window() {
    assert_eq!(EXTERNAL_RAM, 0x3F80_0000..0x3FC0_0000);
    assert!(!is_external(0x3F7F_FFFF));
    assert!(is_external(0x3F80_0000));
    assert!(is_external(0x3FBF_FFFF));
    assert!(!is_external(0x3FC0_0000));
    // Internal data RAM and the reclaimed region.
    assert!(!is_external(0x3FFB_0000));
    assert!(!is_external(0x3FFE_0000));
}

#[test]
fn a_field_is_located_in_its_little_endian_word() {
    let word = 0x4433_2211;
    for offset in 0..4 {
        let field = Field::locate(0x3FFB_0000 + offset, 1);
        assert_eq!(field.word, 0x3FFB_0000);
        assert_eq!(field.extract(word), 0x11 + 0x11 * offset as u32);
    }
    assert_eq!(Field::locate(0x3FFB_0004, 2).extract(word), 0x2211);
    assert_eq!(Field::locate(0x3FFB_0006, 2).extract(word), 0x4433);
    assert_eq!(Field::locate(0x3FFB_0006, 2).word, 0x3FFB_0004);
    assert_eq!(Field::locate(0x3FFB_0008, 4).extract(word), word);
}

#[test]
fn inserting_a_field_keeps_the_other_bytes() {
    let word = 0x4433_2211;
    assert_eq!(Field::locate(1, 1).insert(word, 0xAB), 0x4433_AB11);
    assert_eq!(Field::locate(3, 1).insert(word, 0xAB), 0xAB33_2211);
    assert_eq!(Field::locate(2, 2).insert(word, 0xABCD), 0xABCD_2211);
    assert_eq!(Field::locate(0, 4).insert(word, 0xDEAD_BEEF), 0xDEAD_BEEF);
    // Bits beyond the field do not reach the neighbours.
    assert_eq!(Field::locate(0, 1).insert(word, 0xFFFF_FF00), 0x4433_2200);
    assert_eq!(Field::locate(0, 2).insert(word, 0x1_0000), 0x4433_0000);
}

#[test]
#[should_panic]
fn a_misaligned_field_is_rejected() {
    Field::locate(0x3FFB_0001, 2);
}

#[test]
fn operations_compute_the_stored_value() {
    assert_eq!(Operation::Exchange.apply(5, 9), 9);
    assert_eq!(Operation::Add.apply(u32::MAX, 2), 1);
    assert_eq!(Operation::Subtract.apply(0, 1), u32::MAX);
    assert_eq!(Operation::And.apply(0b1100, 0b1010), 0b1000);
    assert_eq!(Operation::Or.apply(0b1100, 0b1010), 0b1110);
    assert_eq!(Operation::Xor.apply(0b1100, 0b1010), 0b0110);
    assert_eq!(Operation::Nand.apply(0b1100, 0b1010), !0b1000);
}

#[test]
fn an_update_returns_the_old_value_and_wraps_in_the_field() {
    let memory = Memory::new(0x4433_22FF);
    let field = Field::locate(0, 1);
    let old = update(
        field,
        || memory.load(),
        |expected, new| memory.compare_and_set(expected, new),
        |old| Operation::Add.apply(old, 1),
    );
    assert_eq!(old, 0xFF);
    // The byte wraps to zero without carrying into its neighbour.
    assert_eq!(memory.load(), 0x4433_2200);
    assert_eq!(memory.attempts.get(), 1);
}

#[test]
fn an_update_is_retried_when_a_neighbour_changes() {
    let memory = Memory::new(0x0000_0005);
    // Another writer changes the top byte between the load and the store.
    memory.interference.set(Some(0x7700_0005));
    let old = update(
        Field::locate(0, 1),
        || memory.load(),
        |expected, new| memory.compare_and_set(expected, new),
        |old| Operation::Add.apply(old, 1),
    );
    assert_eq!(old, 5);
    assert_eq!(memory.load(), 0x7700_0006);
    assert_eq!(memory.attempts.get(), 2);
}

#[test]
fn an_update_uses_the_value_another_writer_stored() {
    let memory = Memory::new(10);
    memory.interference.set(Some(20));
    let old = update(
        Field::locate(0, 4),
        || memory.load(),
        |expected, new| memory.compare_and_set(expected, new),
        |old| Operation::Add.apply(old, 1),
    );
    assert_eq!(old, 20);
    assert_eq!(memory.load(), 21);
}

#[test]
fn a_compare_exchange_stores_only_on_a_match() {
    let memory = Memory::new(0x4433_2211);
    let field = Field::locate(2, 2);
    let load = || memory.load();
    let compare_and_set = |expected, new| memory.compare_and_set(expected, new);
    assert_eq!(
        compare_exchange(field, load, compare_and_set, 0x1234, 0xABCD),
        0x4433
    );
    assert_eq!(memory.load(), 0x4433_2211);
    assert_eq!(memory.attempts.get(), 0);
    assert_eq!(
        compare_exchange(field, load, compare_and_set, 0x4433, 0xABCD),
        0x4433
    );
    assert_eq!(memory.load(), 0xABCD_2211);
}

#[test]
fn a_compare_exchange_ignores_bits_beyond_the_field() {
    let memory = Memory::new(0x0000_0001);
    // The compiler does not extend a `bool` or `u8` argument.
    let old = compare_exchange(
        Field::locate(0, 1),
        || memory.load(),
        |expected, new| memory.compare_and_set(expected, new),
        0xFFFF_FF01,
        0xFFFF_FF00,
    );
    assert_eq!(old, 1);
    assert_eq!(memory.load(), 0);
}

#[test]
fn a_compare_exchange_is_retried_when_only_a_neighbour_changes() {
    let memory = Memory::new(0x0000_0001);
    memory.interference.set(Some(0x0055_0001));
    let old = compare_exchange(
        Field::locate(0, 1),
        || memory.load(),
        |expected, new| memory.compare_and_set(expected, new),
        1,
        2,
    );
    assert_eq!(old, 1);
    assert_eq!(memory.load(), 0x0055_0002);
    assert_eq!(memory.attempts.get(), 2);
}

#[test]
fn a_compare_exchange_fails_when_the_value_itself_changes() {
    let memory = Memory::new(1);
    memory.interference.set(Some(3));
    let old = compare_exchange(
        Field::locate(0, 4),
        || memory.load(),
        |expected, new| memory.compare_and_set(expected, new),
        1,
        2,
    );
    assert_eq!(old, 3);
    assert_eq!(memory.load(), 3);
}
