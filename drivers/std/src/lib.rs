#![cfg(any(target_os = "linux", target_os = "macos", target_os = "windows"))]

pub mod console;
#[cfg(feature = "crash")]
pub mod crash;
pub mod devices;
pub mod drive_file;
#[cfg(unix)]
pub mod executable_memory;
pub mod executor;
#[cfg(all(feature = "heaptrack", target_os = "linux"))]
mod heaptrack;
// The uinput ioctl request numbers use the generic ioctl layout of the Linux kernel
// (direction in bits 30-31, size in bits 16-29, type in bits 8-15, number in bits 0-7),
// which the kernel uses on these architectures only.
#[cfg(all(
    feature = "human-interface",
    target_os = "linux",
    any(
        target_arch = "x86",
        target_arch = "x86_64",
        target_arch = "arm",
        target_arch = "aarch64",
        target_arch = "riscv32",
        target_arch = "riscv64"
    )
))]
pub mod human_interface;
pub mod io;
pub mod loader;
pub mod log;
pub mod memory;
pub mod power;
#[cfg(all(feature = "profiling", target_os = "linux"))]
pub mod profiling;
pub mod tuntap;

pub extern crate memory as memory_exported;
