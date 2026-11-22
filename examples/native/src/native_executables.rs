//! The executable formats the example registers besides its static executables.
//!
//! The `native-dev` feature adds the developer handler for raw ELF files, backed
//! by the host's executable memory. It runs any native image it can reach without
//! sandboxing, so enable it only on a trusted file system.

use xila::executable::ExecutableFormat;

#[cfg(all(
    feature = "native-dev",
    not(all(
        target_os = "linux",
        any(target_arch = "x86_64", target_arch = "aarch64")
    ))
))]
compile_error!("The `native-dev` feature needs Linux on x86-64 or AArch64.");

#[cfg(feature = "native-dev")]
mod native {
    use drivers_std::executable_memory::ExecutableMemory;
    use xila::executable::NativeFormat;

    /// Usable coroutine stack of a native executable.
    const STACK_SIZE: usize = 256 * 1024;
    /// Largest ELF file the handler reads.
    const FILE_LIMIT: usize = 16 * 1024 * 1024;
    /// Largest installed image.
    const IMAGE_LIMIT: usize = 16 * 1024 * 1024;

    static MEMORY: ExecutableMemory = ExecutableMemory::new();

    // SAFETY: the handler exists only in developer builds, which run trusted ELF
    // files built for the current startup ABI from the host file system. The std
    // executor never moves a task to another thread.
    pub static FORMAT: NativeFormat =
        unsafe { NativeFormat::new(&MEMORY, STACK_SIZE, FILE_LIMIT, IMAGE_LIMIT) };
}

/// The formats to put in the executable context.
#[cfg(feature = "native-dev")]
pub static FORMATS: &[ExecutableFormat] = &[native::FORMAT.registration()];
/// The formats to put in the executable context.
#[cfg(not(feature = "native-dev"))]
pub static FORMATS: &[ExecutableFormat] = &[];
