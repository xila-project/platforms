//! Main-core stack instrumentation for firmware using esp-hal's linker script.

use core::{cell::Cell, ptr};

use critical_section::Mutex;

unsafe extern "C" {
    static _stack_end_cpu0: u32;
    static _stack_start_cpu0: u32;
    static __stack_chk_guard: u32;
}

const PATTERN: u32 = 0xa5c3_9e71;

// Leave room for the Xtensa register-window spill area below the live stack
// pointer, and for any stack activity while painting.
const PAINT_MARGIN: usize = 256;
static PAINT_END: Mutex<Cell<usize>> = Mutex::new(Cell::new(0));

/// Paints the unused part of the main stack once, after esp-hal initialization.
pub(super) fn paint() {
    critical_section::with(|section| {
        let painted_end = PAINT_END.borrow(section);
        if painted_end.get() != 0 {
            return;
        }

        let stack_bottom = (&raw const _stack_end_cpu0).addr();
        let stack_top = (&raw const _stack_start_cpu0).addr();
        let guard = (&raw const __stack_chk_guard).addr();
        let stack_pointer: usize;
        // SAFETY: Read the current core's stack register without modifying it.
        unsafe {
            #[cfg(target_arch = "xtensa")]
            core::arch::asm!("mov {0}, sp", out(reg) stack_pointer, options(nomem, nostack));
            #[cfg(target_arch = "riscv32")]
            core::arch::asm!("mv {0}, sp", out(reg) stack_pointer, options(nomem, nostack));
        }
        if stack_pointer <= stack_bottom || stack_pointer > stack_top {
            return;
        }
        let limit = stack_pointer.saturating_sub(PAINT_MARGIN) & !3;
        if limit <= stack_bottom {
            return;
        }

        for address in (stack_bottom..limit).step_by(size_of::<u32>()) {
            if address != guard {
                // SAFETY: This aligned word is in unused main-core stack
                // space, below the reserved margin, with interrupts masked.
                unsafe { ptr::with_exposed_provenance_mut::<u32>(address).write_volatile(PATTERN) };
            }
        }
        painted_end.set(limit);
    });
}

/// Returns estimated bytes used and total bytes from the early boot paint.
pub(super) fn high_water() -> Option<(usize, usize)> {
    critical_section::with(|section| {
        let limit = PAINT_END.borrow(section).get();
        if limit == 0 {
            return None;
        }
        let bottom = (&raw const _stack_end_cpu0).addr();
        let top = (&raw const _stack_start_cpu0).addr();
        let guard = (&raw const __stack_chk_guard).addr();
        let first_changed = (bottom..limit)
            .step_by(size_of::<u32>())
            .find(|&address| {
                address != guard
                    // SAFETY: This word was painted with a valid value. Read
                    // without creating a reference to the stack.
                    && unsafe { ptr::with_exposed_provenance::<u32>(address).read_volatile() }
                        != PATTERN
            })
            .unwrap_or(limit);
        Some((top - first_changed, top - bottom))
    })
}
