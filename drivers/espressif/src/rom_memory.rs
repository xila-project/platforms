//! `memcpy`, `memmove` and `memset` that keep the ROM routines from writing
//! to PSRAM on the ESP32.
//!
//! esp-hal links these three functions to the routines in the chip's ROM.
//! The ROM was not built with the workaround for the PSRAM cache erratum of
//! ESP32 revision v1 (CPU-3.9: a write to PSRAM followed closely by a read of
//! the same address can be lost). A ROM routine does not read back what it
//! wrote, but its last write can be followed by a read in its caller, with
//! nothing in between. ESP-IDF therefore does not use the ROM's C library
//! when it enables the workaround.
//!
//! The board links with `--wrap=memcpy`, `--wrap=memmove` and `--wrap=memset`
//! (see its `build.rs`), so every call from Rust and C code comes here. A
//! call that writes to PSRAM runs a loop built with the workaround; any other
//! call goes to the ROM routine, as before. Reads need no such care: the ROM
//! routines that only read PSRAM run many instructions before their first
//! read.
//!
//! The functions are in RAM, like the ROM routines they stand in for: code
//! that runs while the flash cache is off, such as esp-hal's PSRAM
//! initialization, calls them.

mod loops;

use crate::atomics::EXTERNAL_RAM;

unsafe extern "C" {
    fn __real_memcpy(destination: *mut u8, source: *const u8, length: usize) -> *mut u8;
    fn __real_memmove(destination: *mut u8, source: *const u8, length: usize) -> *mut u8;
    fn __real_memset(destination: *mut u8, value: i32, length: usize) -> *mut u8;
}

/// # Safety
///
/// As for C's `memcpy`.
#[esp_hal::ram]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn __wrap_memcpy(
    destination: *mut u8,
    source: *const u8,
    length: usize,
) -> *mut u8 {
    if loops::overlaps(destination as usize, length, &EXTERNAL_RAM) {
        // SAFETY: the caller provides valid, non-overlapping ranges.
        unsafe { loops::copy_forward(destination, source, length) };
        destination
    } else {
        // SAFETY: as above.
        unsafe { __real_memcpy(destination, source, length) }
    }
}

/// # Safety
///
/// As for C's `memmove`.
#[esp_hal::ram]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn __wrap_memmove(
    destination: *mut u8,
    source: *const u8,
    length: usize,
) -> *mut u8 {
    if !loops::overlaps(destination as usize, length, &EXTERNAL_RAM) {
        // SAFETY: the caller provides valid ranges.
        return unsafe { __real_memmove(destination, source, length) };
    }
    // SAFETY: the caller provides valid ranges; the direction keeps the
    // bytes of an overlap from being overwritten before they are read.
    unsafe {
        if (destination as usize) <= source as usize {
            loops::copy_forward(destination, source, length);
        } else {
            loops::copy_backward(destination, source, length);
        }
    }
    destination
}

/// # Safety
///
/// As for C's `memset`.
#[esp_hal::ram]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn __wrap_memset(destination: *mut u8, value: i32, length: usize) -> *mut u8 {
    if loops::overlaps(destination as usize, length, &EXTERNAL_RAM) {
        // SAFETY: the caller provides a valid range.
        unsafe { loops::fill(destination, value as u8, length) };
        destination
    } else {
        // SAFETY: as above.
        unsafe { __real_memset(destination, value, length) }
    }
}
