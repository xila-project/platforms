//! Copy and fill loops for memory that the ROM routines must not write.
//!
//! The loops use volatile accesses so that the compiler keeps them as loops:
//! it would otherwise turn them back into calls to `memcpy` and `memset`,
//! which are the functions that call them. Every function is inlined into
//! its caller, which runs from RAM.

use core::ops::Range;

/// Whether the `length` bytes at `start` overlap `range`.
#[inline(always)]
pub const fn overlaps(start: usize, length: usize, range: &Range<usize>) -> bool {
    length != 0 && start < range.end && start.saturating_add(length) > range.start
}

const WORD: usize = size_of::<u32>();

/// Copies `length` bytes from `source` to `destination`, lowest address
/// first. Correct for overlapping ranges when `destination` is below `source`.
///
/// # Safety
///
/// `source` must be valid for reads and `destination` for writes of `length`
/// bytes.
#[inline(always)]
pub unsafe fn copy_forward(mut destination: *mut u8, mut source: *const u8, mut length: usize) {
    // Word copies need both pointers aligned, which they can only be together.
    let together = (destination as usize ^ source as usize).is_multiple_of(WORD);
    // SAFETY: every access stays inside the `length` bytes of the caller.
    unsafe {
        while length > 0 && !(together && (destination as usize).is_multiple_of(WORD)) {
            destination.write_volatile(source.read_volatile());
            destination = destination.add(1);
            source = source.add(1);
            length -= 1;
        }
        while length >= WORD {
            destination
                .cast::<u32>()
                .write_volatile(source.cast::<u32>().read_volatile());
            destination = destination.add(WORD);
            source = source.add(WORD);
            length -= WORD;
        }
        while length > 0 {
            destination.write_volatile(source.read_volatile());
            destination = destination.add(1);
            source = source.add(1);
            length -= 1;
        }
    }
}

/// Copies `length` bytes from `source` to `destination`, highest address
/// first. Correct for overlapping ranges when `destination` is above `source`.
///
/// # Safety
///
/// `source` must be valid for reads and `destination` for writes of `length`
/// bytes.
#[inline(always)]
pub unsafe fn copy_backward(destination: *mut u8, source: *const u8, mut length: usize) {
    let together = (destination as usize ^ source as usize).is_multiple_of(WORD);
    // SAFETY: every access stays inside the `length` bytes of the caller.
    unsafe {
        let mut destination = destination.add(length);
        let mut source = source.add(length);
        while length > 0 && !(together && (destination as usize).is_multiple_of(WORD)) {
            destination = destination.sub(1);
            source = source.sub(1);
            destination.write_volatile(source.read_volatile());
            length -= 1;
        }
        while length >= WORD {
            destination = destination.sub(WORD);
            source = source.sub(WORD);
            destination
                .cast::<u32>()
                .write_volatile(source.cast::<u32>().read_volatile());
            length -= WORD;
        }
        while length > 0 {
            destination = destination.sub(1);
            source = source.sub(1);
            destination.write_volatile(source.read_volatile());
            length -= 1;
        }
    }
}

/// Sets `length` bytes at `destination` to `value`.
///
/// # Safety
///
/// `destination` must be valid for writes of `length` bytes.
#[inline(always)]
pub unsafe fn fill(mut destination: *mut u8, value: u8, mut length: usize) {
    let word = u32::from_ne_bytes([value; WORD]);
    // SAFETY: every access stays inside the `length` bytes of the caller.
    unsafe {
        while length > 0 && !(destination as usize).is_multiple_of(WORD) {
            destination.write_volatile(value);
            destination = destination.add(1);
            length -= 1;
        }
        while length >= WORD {
            destination.cast::<u32>().write_volatile(word);
            destination = destination.add(WORD);
            length -= WORD;
        }
        while length > 0 {
            destination.write_volatile(value);
            destination = destination.add(1);
            length -= 1;
        }
    }
}
