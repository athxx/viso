//! What a texture or surface holds, in its own format, and how each format
//! takes a row of premultiplied linear color.
//!
//! - 8-bit unorm (`Bgra8Unorm`, `Rgba8Unorm`, `Rgba8Data`): four bytes per
//!   pixel in the format's byte order, so a BGRA surface is the XRGB8888 /
//!   ARGB8888 scanout layout as is. A write rounds the source to bytes and
//!   composites in integers ([`super::span`]), as an 8-bit attachment does.
//! - `Rgb565Unorm`: two bytes per pixel, opaque. A write widens the
//!   destination to bytes, composites, and narrows back — rounded, or through
//!   a 4×4 ordered dither for the draws that ask for it (gradients), so a
//!   ramp does not band into 32 steps.
//! - `R8Unorm`: one coverage byte per texel (glyph and mask atlases), sampled
//!   as premultiplied white; never a render target.
//! - `Rgba16Float`: linear `f32` RGBA, unclamped, for HDR targets.
//! - `Rgba4444Unorm`, `Rgba5551Unorm`: two bytes per texel as uploaded,
//!   sampled only (16-bit images).
//!
//! A render target's pixels are allocated by the first pass that draws into
//! it, an uploaded texture's by its first write. A texture created over
//! static bytes ([`Pixels::Static`]) borrows them for its life — an image in
//! the binary, read in place from flash — and is never written. The raster
//! samples no block-compressed format: an image in one is decoded to RGBA8
//! before it reaches here (`Caps::sampled`).

use super::span;
use crate::resource::{TextureFormat, f16_to_f32};
use crate::{CpuTexels, Packed16, rgb565_to_8};

/// A texture's or surface's pixels.
#[derive(Debug, Default)]
pub(super) enum Pixels {
    /// Nothing written yet: reads as transparent black.
    #[default]
    None,
    Unorm8(Vec<[u8; 4]>),
    R8(Vec<u8>),
    Rgb565(Vec<u16>),
    /// 16-bit texels of a sampled-only format, little-endian.
    Packed16(Vec<[u8; 2]>),
    Float(Vec<[f32; 4]>),
    /// A depth attachment's values.
    Depth(Vec<f32>),
    /// Texels in the texture's format, borrowed in place.
    Static(&'static [u8]),
}

/// Why the raster refuses a format: it samples none of the block-compressed
/// ones.
const COMPRESSED: &str = "the CPU raster samples no block-compressed texture: decode it to RGBA8 \
                          first (Caps::sampled)";

impl Pixels {
    /// `len` pixels of `format`, all transparent black (opaque black for a
    /// format without alpha).
    pub(super) fn zeroed(format: TextureFormat, len: usize) -> Pixels {
        match format {
            TextureFormat::Bgra8Unorm | TextureFormat::Rgba8Unorm | TextureFormat::Rgba8Data => {
                Pixels::Unorm8(vec![[0; 4]; len])
            }
            TextureFormat::R8Unorm => Pixels::R8(vec![0; len]),
            TextureFormat::Rgb565Unorm => Pixels::Rgb565(vec![0; len]),
            TextureFormat::Rgba4444Unorm | TextureFormat::Rgba5551Unorm => {
                Pixels::Packed16(vec![[0; 2]; len])
            }
            TextureFormat::Rgba16Float => Pixels::Float(vec![[0.0; 4]; len]),
            TextureFormat::Depth32Float => Pixels::Depth(vec![0.0; len]),
            compressed => panic!("{COMPRESSED}, not {compressed:?}"),
        }
    }

    /// `len` pixels of `format`, every one `color`, stored as a write of it.
    pub(super) fn filled(format: TextureFormat, len: usize, color: [f32; 4]) -> Pixels {
        let fmt = Fmt::of(format);
        match format {
            TextureFormat::Bgra8Unorm | TextureFormat::Rgba8Unorm | TextureFormat::Rgba8Data => {
                Pixels::Unorm8(vec![span::pack(color, fmt.bgra); len])
            }
            TextureFormat::R8Unorm => Pixels::R8(vec![span::pack1(color[3]); len]),
            TextureFormat::Rgb565Unorm => Pixels::Rgb565(vec![<u16 as Px>::store(color, fmt); len]),
            TextureFormat::Rgba16Float => Pixels::Float(vec![color; len]),
            TextureFormat::Depth32Float => Pixels::Depth(vec![color[0]; len]),
            other => panic!("a {other:?} texture is not a render target"),
        }
    }

    /// The bytes of storage held.
    pub(super) fn bytes(&self) -> usize {
        match self {
            Pixels::None => 0,
            Pixels::Unorm8(p) => p.len() * 4,
            Pixels::R8(p) => p.len(),
            Pixels::Rgb565(p) => p.len() * 2,
            Pixels::Packed16(p) => p.len() * 2,
            Pixels::Float(p) => p.len() * 16,
            Pixels::Depth(p) => p.len() * 4,
            // Borrowed: the raster holds none of it.
            Pixels::Static(_) => 0,
        }
    }

    /// The pixels as the samplers read them.
    pub(super) fn texels(&self, format: TextureFormat) -> CpuTexels<'_> {
        match self {
            Pixels::None => CpuTexels::None,
            Pixels::Unorm8(texels) => CpuTexels::Rgba8 {
                texels,
                bgra: format == TextureFormat::Bgra8Unorm,
            },
            Pixels::R8(texels) => CpuTexels::R8(texels),
            Pixels::Rgb565(texels) => CpuTexels::Rgb565(texels),
            Pixels::Packed16(texels) => CpuTexels::Packed16 {
                texels,
                layout: Packed16::of(format).expect("a 16-bit format"),
            },
            Pixels::Float(texels) => CpuTexels::Float(texels),
            // Depth is drawn against, never sampled.
            Pixels::Depth(_) => CpuTexels::None,
            Pixels::Static(bytes) => Self::borrowed(format, bytes),
        }
    }

    /// `bytes`, laid out in `format`, as the samplers read them in place.
    fn borrowed(format: TextureFormat, bytes: &[u8]) -> CpuTexels<'_> {
        match format {
            TextureFormat::Bgra8Unorm | TextureFormat::Rgba8Unorm | TextureFormat::Rgba8Data => {
                CpuTexels::Rgba8 {
                    texels: bytes.as_chunks::<4>().0,
                    bgra: format == TextureFormat::Bgra8Unorm,
                }
            }
            TextureFormat::R8Unorm => CpuTexels::R8(bytes),
            TextureFormat::Rgb565Unorm
            | TextureFormat::Rgba4444Unorm
            | TextureFormat::Rgba5551Unorm => CpuTexels::Packed16 {
                texels: bytes.as_chunks::<2>().0,
                layout: Packed16::of(format).expect("a 16-bit format"),
            },
            other => panic!("the CPU raster reads no {other:?} texture in place"),
        }
    }

    /// Whether static bytes of `format` can be sampled in place.
    pub(super) fn borrows(format: TextureFormat) -> bool {
        matches!(
            format,
            TextureFormat::Bgra8Unorm
                | TextureFormat::Rgba8Unorm
                | TextureFormat::Rgba8Data
                | TextureFormat::R8Unorm
                | TextureFormat::Rgb565Unorm
                | TextureFormat::Rgba4444Unorm
                | TextureFormat::Rgba5551Unorm
        )
    }

    /// Writes `bytes`, laid out in `format` row-major, at `(x, y, w, h)` of a
    /// `width`-wide image of `len` pixels, allocating on first write.
    #[allow(clippy::too_many_arguments)]
    pub(super) fn write(
        &mut self,
        format: TextureFormat,
        len: usize,
        width: u32,
        (x, y, w, h): (u32, u32, u32, u32),
        bytes: &[u8],
    ) {
        if matches!(self, Pixels::None) {
            *self = Pixels::zeroed(format, len);
        }
        let bpt = format.bytes_per_texel();
        for row in 0..h as usize {
            let src = &bytes[row * w as usize * bpt..(row + 1) * w as usize * bpt];
            let at = (y as usize + row) * width as usize + x as usize;
            match self {
                Pixels::Unorm8(p) => {
                    let (texels, _) = src.as_chunks::<4>();
                    p[at..at + w as usize].copy_from_slice(texels);
                }
                Pixels::R8(p) => p[at..at + w as usize].copy_from_slice(src),
                Pixels::Rgb565(p) => {
                    let (texels, _) = src.as_chunks::<2>();
                    for (d, s) in p[at..at + w as usize].iter_mut().zip(texels) {
                        *d = u16::from_le_bytes(*s);
                    }
                }
                Pixels::Packed16(p) => p[at..at + w as usize].copy_from_slice(src.as_chunks().0),
                Pixels::Float(p) => {
                    for (d, s) in p[at..at + w as usize].iter_mut().zip(src.chunks_exact(bpt)) {
                        *d = if format == TextureFormat::Rgba16Float {
                            std::array::from_fn(|c| {
                                f16_to_f32(u16::from_le_bytes([s[c * 2], s[c * 2 + 1]]))
                            })
                        } else {
                            [0.0; 4]
                        };
                    }
                }
                Pixels::Depth(p) => {
                    let (texels, _) = src.as_chunks::<4>();
                    for (d, s) in p[at..at + w as usize].iter_mut().zip(texels) {
                        *d = f32::from_le_bytes(*s);
                    }
                }
                Pixels::Static(_) => panic!("a texture over static bytes is never written"),
                Pixels::None => unreachable!("allocated above"),
            }
        }
    }
}

/// How a format lays its channels out.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct Fmt {
    /// Bytes in BGRA order.
    pub bgra: bool,
}

impl Fmt {
    pub(super) fn of(format: TextureFormat) -> Fmt {
        Fmt {
            bgra: format == TextureFormat::Bgra8Unorm,
        }
    }
}

/// The 4×4 Bayer matrix, as thresholds in `0..255` for the ordered dither.
const BAYER: [[u8; 4]; 4] = {
    const M: [[u8; 4]; 4] = [[0, 8, 2, 10], [12, 4, 14, 6], [3, 11, 1, 9], [15, 7, 13, 5]];
    let mut out = [[0u8; 4]; 4];
    let mut y = 0;
    while y < 4 {
        let mut x = 0;
        while x < 4 {
            out[y][x] = M[y][x] * 16 + 8;
            x += 1;
        }
        y += 1;
    }
    out
};

/// One stored pixel of a render-target format.
pub(super) trait Px: Copy + Send + Sync + 'static {
    /// `src` (premultiplied linear, one per pixel) composited source-over
    /// into `dst`; `dither` is the row's `(x, y)` when the draw dithers.
    fn blend(dst: &mut [Self], src: &[[f32; 4]], fmt: Fmt, dither: Option<(u32, u32)>);

    /// `src` written over `dst` as is.
    fn replace(dst: &mut [Self], src: &[[f32; 4]], fmt: Fmt);

    /// `src` read as premultiplied linear color into `out`.
    fn load(src: &[Self], out: &mut [[f32; 4]], fmt: Fmt);

    /// `color` as stored.
    fn store(color: [f32; 4], fmt: Fmt) -> Self;
}

impl Px for [u8; 4] {
    #[inline]
    fn blend(dst: &mut [[u8; 4]], src: &[[f32; 4]], fmt: Fmt, _: Option<(u32, u32)>) {
        let mut packed = [[0u8; 4]; super::TILE as usize];
        for (dst, src) in dst.chunks_mut(packed.len()).zip(src.chunks(packed.len())) {
            let packed = &mut packed[..src.len()];
            span::pack_row(src, packed, fmt.bgra);
            span::over_row(dst, packed);
        }
    }

    #[inline]
    fn replace(dst: &mut [[u8; 4]], src: &[[f32; 4]], fmt: Fmt) {
        span::pack_row(src, dst, fmt.bgra);
    }

    fn load(src: &[[u8; 4]], out: &mut [[f32; 4]], fmt: Fmt) {
        let texels = CpuTexels::Rgba8 {
            texels: src,
            bgra: fmt.bgra,
        };
        for (i, o) in out.iter_mut().enumerate() {
            *o = texels.get(i);
        }
    }

    fn store(color: [f32; 4], fmt: Fmt) -> [u8; 4] {
        span::pack(color, fmt.bgra)
    }
}

/// A byte channel narrowed to `bits` bits, rounding at threshold `t` of
/// `0..255` (`127` rounds to nearest).
#[inline(always)]
fn narrow(v: u8, bits: u32, t: u32) -> u16 {
    let max = (1 << bits) - 1;
    ((u32::from(v) * max + t) / 255) as u16
}

#[inline(always)]
fn to_565([r, g, b]: [u8; 3], t: u32) -> u16 {
    (narrow(r, 5, t) << 11) | (narrow(g, 6, t) << 5) | narrow(b, 5, t)
}

impl Px for u16 {
    fn blend(dst: &mut [u16], src: &[[f32; 4]], _: Fmt, dither: Option<(u32, u32)>) {
        let mut packed = [[0u8; 4]; super::TILE as usize];
        for (chunk, (dst, src)) in dst
            .chunks_mut(packed.len())
            .zip(src.chunks(packed.len()))
            .enumerate()
        {
            let packed = &mut packed[..src.len()];
            span::pack_row(src, packed, false);
            for (i, (d, s)) in dst.iter_mut().zip(packed.iter()).enumerate() {
                let [r, g, b] = rgb565_to_8(*d);
                let [r, g, b, _] = span::over1([r, g, b, 255], *s);
                let t = dither.map_or(127, |(x, y)| {
                    let x = x as usize + chunk * super::TILE as usize + i;
                    u32::from(BAYER[y as usize & 3][x & 3])
                });
                *d = to_565([r, g, b], t);
            }
        }
    }

    fn replace(dst: &mut [u16], src: &[[f32; 4]], fmt: Fmt) {
        for (d, &s) in dst.iter_mut().zip(src) {
            *d = Self::store(s, fmt);
        }
    }

    fn load(src: &[u16], out: &mut [[f32; 4]], _: Fmt) {
        let texels = CpuTexels::Rgb565(src);
        for (i, o) in out.iter_mut().enumerate() {
            *o = texels.get(i);
        }
    }

    fn store(color: [f32; 4], _: Fmt) -> u16 {
        let [r, g, b, _] = span::pack(color, false);
        to_565([r, g, b], 127)
    }
}

impl Px for [f32; 4] {
    fn blend(dst: &mut [[f32; 4]], src: &[[f32; 4]], _: Fmt, _: Option<(u32, u32)>) {
        for (d, s) in dst.iter_mut().zip(src) {
            let inv = 1.0 - s[3];
            *d = std::array::from_fn(|c| s[c] + d[c] * inv);
        }
    }

    fn replace(dst: &mut [[f32; 4]], src: &[[f32; 4]], _: Fmt) {
        dst.copy_from_slice(src);
    }

    fn load(src: &[[f32; 4]], out: &mut [[f32; 4]], _: Fmt) {
        out.copy_from_slice(src);
    }

    fn store(color: [f32; 4], _: Fmt) -> [f32; 4] {
        color
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rgb565_round_trips_its_own_values_and_rounds_bytes() {
        for v in [0u16, 0xffff, 0xf800, 0x07e0, 0x001f, 0x1234] {
            let mut d = [v];
            <u16 as Px>::blend(&mut d, &[[0.0; 4]], Fmt { bgra: false }, None);
            assert_eq!(d[0], v, "a transparent source keeps {v:#06x}");
        }
        assert_eq!(
            <u16 as Px>::store([1.0, 1.0, 1.0, 1.0], Fmt { bgra: false }),
            0xffff
        );
        assert_eq!(
            <u16 as Px>::store([0.0, 0.0, 0.0, 1.0], Fmt { bgra: false }),
            0
        );
    }

    #[test]
    fn a_dithered_ramp_alternates_where_a_rounded_one_bands() {
        // A level between two 5-bit steps: rounding picks one everywhere, the
        // dither mixes both over the 4×4 cell.
        let level = (4.5 * 255.0 / 31.0) / 255.0;
        let src = [[level, 0.0, 0.0, 1.0]; 4];
        let fmt = Fmt { bgra: false };
        let mut seen = std::collections::BTreeSet::new();
        for y in 0..4 {
            let mut row = [0u16; 4];
            <u16 as Px>::blend(&mut row, &src, fmt, Some((0, y)));
            seen.extend(row.iter().map(|v| v >> 11));
        }
        assert_eq!(seen.into_iter().collect::<Vec<_>>(), [4, 5]);
        let mut row = [0u16; 4];
        <u16 as Px>::blend(&mut row, &src, fmt, None);
        assert!(row.iter().all(|&v| v == row[0]));
    }
}
