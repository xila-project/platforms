//! The developer-only handler for raw ELF files (`native-dev` feature).
//!
//! Images live in a dedicated arena in `.dram2_uninit`, the memory the second
//! stage bootloader leaves after start-up: esp-hal protects the rest of the RAM
//! against execution on RISC-V, and places `.dram2_uninit` cleanly inside
//! internal D/IRAM on ESP32-S2 and ESP32-S3 (see
//! `drivers_espressif::executable_memory`).

use core::{mem::MaybeUninit, ptr::addr_of_mut};

use drivers_espressif::executable_memory::ExecutableMemory;
use xila::executable::{ExecutableFormat, NativeFormat};

// `drivers_espressif::executable_memory` exists only on the chips whose internal
// SRAM can execute loaded images: the ESP32-C5, C6, C61, H2, P4, S2 and S3.

/// Size of the arena: `.dram2_uninit` holds 64 KiB on the ESP32-C6, 68 KiB on
/// the ESP32-P4, 72 KiB on the ESP32-S3 and 136 KiB on the ESP32-S2.
const ARENA_SIZE: usize = 48 * 1024;
/// Usable coroutine stack of a native executable.
const STACK_SIZE: usize = 16 * 1024;
/// Largest ELF file the handler reads.
const FILE_LIMIT: usize = 64 * 1024;
/// Largest installed image.
const IMAGE_LIMIT: usize = ARENA_SIZE;

#[unsafe(link_section = ".dram2_uninit")]
static mut ARENA: [MaybeUninit<u8>; ARENA_SIZE] = [MaybeUninit::uninit(); ARENA_SIZE];

static MEMORY: ExecutableMemory = ExecutableMemory::new();

// SAFETY: the handler exists only in developer builds, which run trusted ELF files
// built for the current startup ABI. The firmware registers the per-core ID hook
// at start-up and never runs tasks from an interrupt.
static FORMAT: NativeFormat =
    unsafe { NativeFormat::new(&MEMORY, STACK_SIZE, FILE_LIMIT, IMAGE_LIMIT) };

/// The formats to put in the executable context.
pub static FORMATS: &[ExecutableFormat] = &[FORMAT.registration()];

/// Hand the arena to the executable memory. Call it once, at start-up.
pub fn initialize() {
    // SAFETY: the arena is internal SRAM that nothing else uses, and this runs once.
    let initialized = unsafe { MEMORY.initialize_from_slice(&mut *addr_of_mut!(ARENA)) };
    assert!(
        initialized,
        "The executable arena is invalid or overlaps protected memory"
    );
}

/// Runs [`self_test`] and logs the result, as a board's memory check.
pub fn log_self_test(_: &crate::MemoryReport) {
    match self_test() {
        Ok(address) => xila::log::information!(
            "Xila: native executable memory self-test passed at {address:#x}"
        ),
        Err(error) => {
            xila::log::error!("Xila: native executable memory self-test failed: {error}")
        }
    }
}

/// Write a tiny function returning `42` into the arena, finalize it and call it
/// through its execution-bus address, which proves that the CPU may execute
/// from the arena. Returns the execution address of the code.
pub fn self_test() -> Result<usize, &'static str> {
    use core::alloc::Layout;
    use xila::executable::native_loader::{ExecutableMemory as _, Region};

    #[cfg(target_arch = "riscv32")]
    // addi a0, zero, 42; jalr zero, 0(ra)
    const CODE: [u8; 8] = [0x13, 0x05, 0xa0, 0x02, 0x67, 0x80, 0x00, 0x00];
    #[cfg(target_arch = "xtensa")]
    // entry a1, 32; movi.n a2, 42; retw.n (padded to 8 bytes)
    const CODE: [u8; 8] = [0x36, 0x41, 0x00, 0x2c, 0xa2, 0x1d, 0xf0, 0x00];

    let layout = Layout::from_size_align(CODE.len(), 4).map_err(|_| "invalid layout")?;
    let allocation = MEMORY.allocate(layout).map_err(|_| "allocation failed")?;
    // SAFETY: the allocation is exclusive and holds `layout`.
    unsafe { allocation.cast::<[u8; 8]>().write(CODE) };
    let mut regions = [Region {
        offset: 0,
        size: layout.size(),
        executable: true,
        writable: false,
    }]
    .into_iter();
    let execution = MEMORY.execution_base(allocation);
    // SAFETY: the allocation comes from `allocate` with this layout, and the write is
    // complete.
    let result = unsafe { MEMORY.finish_writes(allocation, layout, &mut regions) }
        .map_err(|_| "finalization failed")
        .map(|()| {
            // SAFETY: the bytes are a complete function of this signature, visible to
            // instruction fetch at `execution`.
            let function: extern "C" fn() -> i32 =
                unsafe { core::mem::transmute(execution.as_ptr()) };
            function()
        });
    // SAFETY: the function returned, and nothing references the allocation any more.
    unsafe { MEMORY.free(allocation, layout) };
    match result? {
        42 => Ok(execution.as_ptr() as usize),
        _ => Err("the code returned a wrong value"),
    }
}
