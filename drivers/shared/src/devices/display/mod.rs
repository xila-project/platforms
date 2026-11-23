//! Displays driven by `mipidsi` (feature `display-mipidsi`).
//!
//! [`ParallelInterface`] adapts a [`ParallelBus`], such as an 8080 bus with DMA,
//! to `mipidsi`'s interface: `mipidsi` owns the panel's initialisation,
//! orientation and address windows, and raw pixel writes reach the bus in one
//! transfer. [`MipidsiScreenDevice`] exposes the result to Xila's graphics as a
//! screen device: each LVGL flush becomes one `set_pixels_raw` call.

mod bus;
mod screen;

pub use bus::*;
pub use screen::*;

#[cfg(test)]
mod tests;
