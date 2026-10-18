//! The CPU-side A8 coverage bitmap both glyph sources produce.

use alloc::vec::Vec;

/// A CPU-side A8 coverage bitmap: one glyph rasterized at a device size.
///
/// Coverage is row-major, one byte per pixel, `0..=255`. The bitmap owns no GPU
/// memory; the caller uploads it into an atlas page and records residency.
#[derive(Debug, Clone, PartialEq)]
pub struct CoverageBitmap {
    /// Bitmap width in device pixels.
    pub width: u32,
    /// Bitmap height in device pixels.
    pub height: u32,
    /// Left edge of the bitmap relative to the pen origin, in device pixels
    /// (the glyph bounding box's `x_min`). Render uses it to place the quad.
    pub left: f32,
    /// Top edge of the bitmap relative to the pen origin, in device pixels
    /// (the glyph bounding box's `y_max`, y-up). Render uses it to place the
    /// quad.
    pub top: f32,
    /// Coverage samples, `width * height` bytes, row-major, top row first.
    pub coverage: Vec<u8>,
}

impl CoverageBitmap {
    /// A zero-area bitmap at `left`, `top`: a space, or an outline with no
    /// contour.
    pub fn empty(left: f32, top: f32) -> Self {
        Self {
            width: 0,
            height: 0,
            left,
            top,
            coverage: Vec::new(),
        }
    }

    /// The upload candidate size in bytes: one byte per covered pixel.
    pub fn byte_len(&self) -> usize {
        self.coverage.len()
    }

    /// Whether the glyph rasterized to no pixels (a space or empty outline).
    pub fn is_empty(&self) -> bool {
        self.coverage.is_empty()
    }
}
