//! The developer-only handler for raw ELF files (`native-dev` feature).
//!
//! Images live in a dedicated arena in `.dram2_uninit`, the memory the second
//! stage bootloader leaves after start-up: esp-hal protects the rest of the RAM
//! against execution (see `drivers_espressif::executable_memory`).

use core::{mem::MaybeUninit, ptr::addr_of_mut};

use drivers_espressif::executable_memory::ExecutableMemory;
use xila::executable::{ExecutableFormat, NativeFormat};

#[cfg(not(any(feature = "esp32c6", feature = "esp32p4")))]
compile_error!("The `native-dev` feature needs the ESP32-C6 or the ESP32-P4.");

/// Size of the arena: `.dram2_uninit` holds 64 KiB on the ESP32-C6 and 68 KiB on
/// the ESP32-P4.
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
// built for the current startup ABI. The executor runs every task on the first core,
// never from an interrupt.
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
        "The executable arena overlaps memory that esp-hal protects"
    );
}

/// Write `li a0, 42; ret` into the arena, finalize it and call it, which proves
/// that the CPU may execute from the arena. Returns the address of the code.
#[cfg(feature = "qemu-esp32c6")]
pub fn self_test() -> Result<usize, &'static str> {
    use core::alloc::Layout;
    use xila::executable::native_loader::{ExecutableMemory as _, Region};

    // addi a0, zero, 42; jalr zero, 0(ra)
    const CODE: [u32; 2] = [0x02a0_0513, 0x0000_8067];

    let layout = Layout::new::<[u32; 2]>();
    let allocation = MEMORY.allocate(layout).map_err(|_| "allocation failed")?;
    // SAFETY: the allocation is exclusive and holds `layout`.
    unsafe { allocation.cast::<[u32; 2]>().write(CODE) };
    let mut regions = [Region {
        offset: 0,
        size: layout.size(),
        executable: true,
        writable: false,
    }]
    .into_iter();
    // SAFETY: the allocation comes from `allocate` with this layout, and the write is
    // complete.
    let result = unsafe { MEMORY.finish_writes(allocation, layout, &mut regions) }
        .map_err(|_| "finalization failed")
        .map(|()| {
            // SAFETY: the bytes are a complete function of this signature, visible to
            // instruction fetch.
            let function: extern "C" fn() -> i32 =
                unsafe { core::mem::transmute(allocation.as_ptr()) };
            function()
        });
    // SAFETY: the function returned, and nothing references the allocation any more.
    unsafe { MEMORY.free(allocation, layout) };
    match result? {
        42 => Ok(allocation.as_ptr() as usize),
        _ => Err("the code returned a wrong value"),
    }
}
