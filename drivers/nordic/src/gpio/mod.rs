//! General Purpose Input/Output (GPIO) driver for Nordic nRF52840.

pub mod logic;
pub use logic::*;

#[cfg(all(target_arch = "arm", target_os = "none"))]
mod device;
#[cfg(all(target_arch = "arm", target_os = "none"))]
pub use device::*;
