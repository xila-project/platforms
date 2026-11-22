/// Maps the controller's native coordinates to screen coordinates, for a
/// screen that is rotated or mirrored relative to the touch panel.
///
/// The axes are swapped first, then mirrored within the screen resolution.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TouchTransform {
    /// Screen width in pixels, after the transform.
    pub width: u16,
    /// Screen height in pixels, after the transform.
    pub height: u16,
    pub swap_axes: bool,
    pub mirror_x: bool,
    pub mirror_y: bool,
}

impl TouchTransform {
    /// No transform, for a `width` × `height` screen.
    pub const fn identity(width: u16, height: u16) -> Self {
        Self {
            width,
            height,
            swap_axes: false,
            mirror_x: false,
            mirror_y: false,
        }
    }

    /// Maps a native point to the screen, clamped to the resolution.
    pub fn apply(&self, x: u16, y: u16) -> (i16, i16) {
        let (x, y) = if self.swap_axes { (y, x) } else { (x, y) };
        let x = x.min(self.width.saturating_sub(1));
        let y = y.min(self.height.saturating_sub(1));
        let x = if self.mirror_x {
            self.width.saturating_sub(1) - x
        } else {
            x
        };
        let y = if self.mirror_y {
            self.height.saturating_sub(1) - y
        } else {
            y
        };
        // The resolution fits in `i16`: screens are far smaller than 32768 pixels.
        (x as i16, y as i16)
    }
}
