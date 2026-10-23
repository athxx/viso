//! The seam a 2D accelerator plugs into: an i.MX PXP, an Allwinner
//! G2D, an STM32 DMA2D. The raster offers it the rectangle work it would
//! otherwise do with the CPU; whatever it declines, the raster does itself,
//! so a device without one, or with one that handles only some formats,
//! draws the same pixels.

/// A target's pixels as stored, row-major, `width` per row.
pub enum FillPixels<'a> {
    /// Four bytes per pixel in the target's byte order — `Bgra8Unorm` and its
    /// sRGB twin are the XRGB8888 / ARGB8888 scanout layout — and the pixel to
    /// fill with.
    Bytes4 {
        pixels: &'a mut [[u8; 4]],
        value: [u8; 4],
    },
    /// RGB565, and the pixel to fill with.
    Rgb565 { pixels: &'a mut [u16], value: u16 },
}

/// One solid fill: `rect` (`[x0, y0, x1, y1)`) of a `width × height` target.
pub struct Fill<'a> {
    pub pixels: FillPixels<'a>,
    pub width: u32,
    pub height: u32,
    pub rect: [u32; 4],
}

/// A 2D accelerator the raster hands work to
/// ([`CpuRaster::set_blitter`](super::CpuRaster::set_blitter)).
///
/// Each operation is synchronous: the pixels are lent for the call, and the
/// raster draws over them as soon as it returns.
pub trait Blitter: Send {
    /// Performs `fill`, or returns `false` to leave it to the raster.
    fn fill(&mut self, fill: Fill<'_>) -> bool;
}
