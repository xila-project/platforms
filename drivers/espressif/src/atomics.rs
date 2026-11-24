//! Atomic operations for the ESP32, whose compare-and-set instruction is not
//! supported on external PSRAM.
//!
//! The Xtensa `S32C1I` instruction is the only atomic read-modify-write that
//! the ESP32 has. Espressif does not support it on PSRAM: ESP-IDF's sources
//! state that it "does not work in external RAM" and send those addresses
//! through a lock instead, without giving the hardware reason, and the report
//! that led to that fix describes race conditions, with no exception. On the
//! classic WT32-SC01, one uncontended instruction on PSRAM compares and
//! stores correctly (the board's `hardware-atomics-probe` feature), so what
//! is not assured is that its read and its write are atomic, which no test
//! on one core can show. A reference count, lock or flag in PSRAM could then
//! be corrupted silently, so PSRAM cannot back the global heap while the
//! compiler emits that instruction inline.
//!
//! The firmware therefore does what ESP-IDF does when it enables PSRAM
//! (`CONFIG_STDATOMIC_S32C1I_SPIRAM_WORKAROUND`, GCC's
//! `-mdisable-hardware-atomics`):
//!
//! - The target's Rust flags disable the `s32c1i` target feature
//!   (`.cargo/config.toml`), so the compiler calls `__atomic_*` functions for
//!   every atomic operation in every crate, `core` and `alloc` included.
//! - This module provides those functions. They look at the address: internal
//!   memory takes the hardware instruction, as before; PSRAM takes a plain
//!   read and write inside a critical section, which masks interrupts and
//!   holds esp-hal's lock against the other core.
//!
//! [`xila_esp32_hardware_compare_and_set`] is the only function of the
//! firmware that uses `S32C1I`; `tools/dev/build_espressif_image.sh` checks
//! this in the ELF file. No address in PSRAM can therefore reach the
//! instruction.
//!
//! The instruction compares with the SCOMPARE1 register, which the exception
//! handlers no longer save once the feature is disabled, so that function
//! masks interrupts while it sets the register and runs the instruction.
//!
//! Loads need no protection: an aligned load of at most 32 bits is atomic.
//! Stores to PSRAM take the critical section too, so that a store cannot be
//! lost in the middle of an emulated read-modify-write.
//!
//! The functions are in RAM, as ESP-IDF's are: interrupt handlers and flash
//! routines that run from RAM use atomic operations, and must not depend on
//! code in flash.

// Without the flag the compiler emits the instruction inline, for PSRAM
// addresses too, and never calls the functions below.
#[cfg(target_feature = "s32c1i")]
compile_error!(
    "build ESP32 firmware with `-C target-feature=-s32c1i` (see .cargo/config.toml): \
     the compare-and-set instruction is not supported on PSRAM"
);

mod word;

use core::arch::asm;

pub use word::{EXTERNAL_RAM, is_external};
use word::{Field, Operation};

/// Stores `new` in the word at `pointer` if it holds `expected`, with the
/// `S32C1I` instruction, and returns what the word held.
///
/// # Safety
///
/// `pointer` must be valid for reads and writes, aligned to 4 bytes and in
/// internal memory: the instruction is not supported on PSRAM.
#[esp_hal::ram]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn xila_esp32_hardware_compare_and_set(
    pointer: *mut u32,
    expected: u32,
    new: u32,
) -> u32 {
    let mut value = new;
    // The assembler refuses the last two instructions while the `s32c1i`
    // feature is disabled, so they are given as bytes, with fixed registers:
    //
    //     wsr.scompare1 a3
    //     s32c1i a4, a2, 0
    //
    // Interrupts are masked around them. Without the feature, the exception
    // handlers of xtensa-lx-rt do not save SCOMPARE1, so a handler that runs
    // between the two instructions and performs an atomic operation of its
    // own would change what this one compares with.
    //
    // SAFETY: the caller provides a valid word in internal memory. The
    // interrupt level is restored to what it was.
    unsafe {
        asm!(
            "rsil {state}, 15",
            ".byte 0x30, 0x0c, 0x13",
            ".byte 0x42, 0xe2, 0x00",
            "wsr.ps {state}",
            "rsync",
            state = out(reg) _,
            in("a2") pointer,
            in("a3") expected,
            inout("a4") value,
            options(nostack),
        );
    }
    value
}

/// A value of an atomic type: 1, 2 or 4 bytes.
trait Value: Copy {
    fn to_word(self) -> u32;
    fn from_word(value: u32) -> Self;
}

macro_rules! implement_value {
    ($($type:ty),*) => {$(
        impl Value for $type {
            #[inline(always)]
            fn to_word(self) -> u32 {
                self as u32
            }

            #[inline(always)]
            fn from_word(value: u32) -> Self {
                value as $type
            }
        }
    )*};
}

implement_value!(u8, u16, u32);

/// Reads the word of `field` and updates it with the hardware instruction.
///
/// # Safety
///
/// The word must be valid for reads and writes and in internal memory.
#[inline(always)]
unsafe fn update_internal(field: Field, update: impl Fn(u32) -> u32) -> u32 {
    let pointer = field.word as *mut u32;
    word::update(
        field,
        // SAFETY: the caller provides a valid word.
        || unsafe { pointer.read_volatile() },
        // SAFETY: the word is valid, aligned and in internal memory.
        |expected, new| unsafe { xila_esp32_hardware_compare_and_set(pointer, expected, new) },
        update,
    )
}

/// Replaces the value at `pointer` by `update(old)` and returns the old value.
///
/// # Safety
///
/// `pointer` must be valid for reads and writes and aligned.
#[inline(always)]
unsafe fn update<T: Value>(pointer: *mut T, update: impl Fn(u32) -> u32) -> u32 {
    if is_external(pointer as usize) {
        critical_section::with(|_| {
            // SAFETY: the caller provides a valid, aligned value. Interrupts
            // are masked and the other core is locked out.
            let old = unsafe { pointer.read_volatile() }.to_word();
            // SAFETY: as above.
            unsafe { pointer.write_volatile(T::from_word(update(old))) };
            old
        })
    } else {
        // SAFETY: the value is valid and in internal memory, and so is the
        // aligned word that holds it.
        unsafe { update_internal(Field::locate(pointer as usize, size_of::<T>()), update) }
    }
}

/// Stores `value` at `pointer`.
///
/// # Safety
///
/// `pointer` must be valid for writes and aligned.
#[inline(always)]
unsafe fn store<T: Value>(pointer: *mut T, value: u32) {
    if is_external(pointer as usize) {
        // SAFETY: the caller provides a valid, aligned value.
        critical_section::with(|_| unsafe { pointer.write_volatile(T::from_word(value)) });
    } else {
        // SAFETY: as above; a store to internal memory is atomic.
        unsafe { pointer.write_volatile(T::from_word(value)) };
    }
}

/// Stores `new` at `pointer` if it holds `expected`, and returns the value it
/// held.
///
/// # Safety
///
/// `pointer` must be valid for reads and writes and aligned.
#[inline(always)]
unsafe fn compare_exchange<T: Value>(pointer: *mut T, expected: u32, new: u32) -> u32 {
    if is_external(pointer as usize) {
        critical_section::with(|_| {
            // SAFETY: the caller provides a valid, aligned value. Interrupts
            // are masked and the other core is locked out.
            let old = unsafe { pointer.read_volatile() }.to_word();
            if old == T::from_word(expected).to_word() {
                // SAFETY: as above.
                unsafe { pointer.write_volatile(T::from_word(new)) };
            }
            old
        })
    } else {
        let field = Field::locate(pointer as usize, size_of::<T>());
        let word = field.word as *mut u32;
        word::compare_exchange(
            field,
            // SAFETY: the value is valid, and so is the word that holds it.
            || unsafe { word.read_volatile() },
            // SAFETY: the word is valid, aligned and in internal memory.
            |expected, new| unsafe { xila_esp32_hardware_compare_and_set(word, expected, new) },
            expected,
            new,
        )
    }
}

/// The functions that the compiler calls for the atomic operations of one
/// size. The memory orderings are ignored: every operation is sequentially
/// consistent. Values travel as 32-bit words, because the callers do not
/// extend smaller arguments.
macro_rules! atomic_functions {
    (
        $type:ty,
        $load:ident,
        $store:ident,
        $exchange:ident,
        $compare_exchange:ident,
        $(($fetch:ident, $operation:ident)),*
    ) => {
        /// # Safety
        ///
        /// `pointer` must be valid for reads and aligned.
        #[esp_hal::ram]
        #[unsafe(no_mangle)]
        pub unsafe extern "C" fn $load(pointer: *const $type, _order: i32) -> u32 {
            // SAFETY: the caller provides a valid, aligned value.
            unsafe { pointer.read_volatile() }.to_word()
        }

        /// # Safety
        ///
        /// `pointer` must be valid for writes and aligned.
        #[esp_hal::ram]
        #[unsafe(no_mangle)]
        pub unsafe extern "C" fn $store(pointer: *mut $type, value: u32, _order: i32) {
            // SAFETY: the caller provides a valid, aligned value.
            unsafe { store(pointer, value) }
        }

        /// # Safety
        ///
        /// `pointer` must be valid for reads and writes and aligned.
        #[esp_hal::ram]
        #[unsafe(no_mangle)]
        pub unsafe extern "C" fn $exchange(pointer: *mut $type, value: u32, _order: i32) -> u32 {
            // SAFETY: the caller provides a valid, aligned value.
            unsafe { update(pointer, |old| Operation::Exchange.apply(old, value)) }
        }

        /// # Safety
        ///
        /// `pointer` and `expected` must be valid for reads and writes and
        /// aligned.
        #[esp_hal::ram]
        #[unsafe(no_mangle)]
        pub unsafe extern "C" fn $compare_exchange(
            pointer: *mut $type,
            expected: *mut $type,
            desired: u32,
            _success: i32,
            _failure: i32,
        ) -> bool {
            // SAFETY: the caller provides a valid expected value.
            let wanted = unsafe { expected.read() };
            // SAFETY: the caller provides a valid, aligned value.
            let old = <$type>::from_word(unsafe {
                compare_exchange(pointer, wanted.to_word(), desired)
            });
            if old == wanted {
                true
            } else {
                // SAFETY: the caller provides a valid expected value.
                unsafe { expected.write(old) };
                false
            }
        }

        $(
            /// # Safety
            ///
            /// `pointer` must be valid for reads and writes and aligned.
            #[esp_hal::ram]
            #[unsafe(no_mangle)]
            pub unsafe extern "C" fn $fetch(pointer: *mut $type, value: u32, _order: i32) -> u32 {
                // SAFETY: the caller provides a valid, aligned value.
                unsafe { update(pointer, |old| Operation::$operation.apply(old, value)) }
            }
        )*
    };
}

atomic_functions!(
    u8,
    __atomic_load_1,
    __atomic_store_1,
    __atomic_exchange_1,
    __atomic_compare_exchange_1,
    (__atomic_fetch_add_1, Add),
    (__atomic_fetch_sub_1, Subtract),
    (__atomic_fetch_and_1, And),
    (__atomic_fetch_or_1, Or),
    (__atomic_fetch_xor_1, Xor),
    (__atomic_fetch_nand_1, Nand)
);
atomic_functions!(
    u16,
    __atomic_load_2,
    __atomic_store_2,
    __atomic_exchange_2,
    __atomic_compare_exchange_2,
    (__atomic_fetch_add_2, Add),
    (__atomic_fetch_sub_2, Subtract),
    (__atomic_fetch_and_2, And),
    (__atomic_fetch_or_2, Or),
    (__atomic_fetch_xor_2, Xor),
    (__atomic_fetch_nand_2, Nand)
);
atomic_functions!(
    u32,
    __atomic_load_4,
    __atomic_store_4,
    __atomic_exchange_4,
    __atomic_compare_exchange_4,
    (__atomic_fetch_add_4, Add),
    (__atomic_fetch_sub_4, Subtract),
    (__atomic_fetch_and_4, And),
    (__atomic_fetch_or_4, Or),
    (__atomic_fetch_xor_4, Xor),
    (__atomic_fetch_nand_4, Nand)
);
