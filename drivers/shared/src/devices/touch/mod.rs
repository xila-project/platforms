//! Capacitive touch controllers (feature `touch-ft6336`).

mod ft6336;
mod transform;

pub use ft6336::*;
pub use transform::*;

/// A touch controller that reports one touch point, in screen coordinates.
pub trait TouchSensor {
    type Error: core::fmt::Debug;

    /// The current touch point, or `None` when nothing touches the panel.
    fn read(&mut self) -> Result<Option<(i16, i16)>, Self::Error>;
}
