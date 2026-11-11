pub mod logic;
pub use logic::*;

#[cfg(all(target_arch = "arm", target_os = "none"))]
mod device;
#[cfg(all(target_arch = "arm", target_os = "none"))]
pub use device::*;
