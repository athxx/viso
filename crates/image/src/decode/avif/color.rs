//! An AV1 picture's samples as straight-alpha sRGB RGBA8.
//!
//! The colour signal — primaries, transfer and matrix, as ITU-T H.273
//! numbers them, and whether the samples use the full range — says how:
//! luma and chroma become R'G'B' through the matrix, chroma upsampled
//! bilinearly from its sites at the centre of the luma it covers. A signal
//! sRGB already shows — BT.709-like primaries and an SDR camera or sRGB
//! curve — stores those values as they are. Any other leaves its curve for
//! linear light, its gamut for sRGB's by matrix, out-of-gamut colours
//! clipped, and, for PQ and HLG, comes down from its peak to SDR white with
//! a curve that keeps the shadows and rolls the highlights off.

use super::av1::{Layout, Picture, Plane, Signal};
use crate::raster::AssetError;
use viso_gpu_abi::srgb;

/// HDR content's reference white, the luminance SDR white stands for.
const SDR_WHITE: f32 = 203.0;

/// The peak an HLG display renders at, and what PQ content without a
/// recorded peak is taken to reach.
const HDR_PEAK: f32 = 1000.0;

/// How the R'G'B' values are made from luma and chroma.
#[derive(Clone, Copy)]
enum Matrix {
    /// `Y'CbCr` by the red and blue luma weights.
    YCbCr {
        kr: f32,
        kb: f32,
    },
    /// The planes are G, B and R.
    Identity,
    YCgCo,
    /// Luma alone: grey.
    Mono,
}

/// The curve between the stored values and linear light.
#[derive(Clone, Copy, PartialEq)]
enum Curve {
    /// sRGB's, or near enough that browsers show it as sRGB: the stored
    /// values are kept.
    Srgb,
    Gamma(f32),
    Linear,
    /// SMPTE ST 2084, in nits.
    Pq,
    /// ARIB STD-B67, scene light.
    Hlg,
}

impl Curve {
    fn of(transfer: u16) -> Curve {
        match transfer {
            4 => Curve::Gamma(2.2),
            5 => Curve::Gamma(2.8),
            8 => Curve::Linear,
            16 => Curve::Pq,
            18 => Curve::Hlg,
            // BT.709, BT.601, SMPTE 240M, sRGB and BT.2020's SDR curves,
            // and unspecified.
            _ => Curve::Srgb,
        }
    }

    /// Stored value `v` as light: relative to SDR white for PQ, scene light
    /// for HLG, display light otherwise.
    fn eotf(self, v: f32) -> f32 {
        match self {
            Curve::Srgb => {
                if v <= 0.04045 {
                    v / 12.92
                } else {
                    ((v + 0.055) / 1.055).powf(2.4)
                }
            }
            Curve::Gamma(g) => v.powf(g),
            Curve::Linear => v,
            Curve::Pq => {
                let (m1, m2) = (2610.0 / 16384.0, 2523.0 / 4096.0 * 128.0);
                let (c1, c2, c3) = (
                    3424.0 / 4096.0,
                    2413.0 / 4096.0 * 32.0,
                    2392.0 / 4096.0 * 32.0,
                );
                let e = v.powf(1.0 / m2);
                let nits = ((e - c1).max(0.0) / (c2 - c3 * e)).powf(1.0 / m1) * 10_000.0;
                nits / SDR_WHITE
            }
            Curve::Hlg => {
                let (a, b, c) = (0.178_832_77, 0.284_668_92, 0.559_910_7);
                if v <= 0.5 {
                    v * v / 3.0
                } else {
                    (((v - c) / a).exp() + b) / 12.0
                }
            }
        }
    }
}

/// A curve tabulated over `[0, 1]` and read with linear interpolation.
struct Table([f32; TABLE + 1]);

const TABLE: usize = 4096;

impl Table {
    fn new(curve: Curve) -> Table {
        Table(std::array::from_fn(|i| curve.eotf(i as f32 / TABLE as f32)))
    }

    #[inline(always)]
    fn at(&self, v: f32) -> f32 {
        let x = v.clamp(0.0, 1.0) * TABLE as f32;
        let i = (x as usize).min(TABLE - 1);
        let t = x - i as f32;
        self.0[i] + (self.0[i + 1] - self.0[i]) * t
    }
}

/// The matrix from linear light in `primaries` to linear sRGB, when they
/// are not sRGB's.
fn gamut(primaries: u16) -> Option<[[f32; 3]; 3]> {
    match primaries {
        // Display P3, and DCI-P3 taken as it.
        11 | 12 => Some([
            [1.224_940_2, -0.224_940_4, 0.0],
            [-0.042_056_955, 1.042_057_1, 0.0],
            [-0.019_637_555, -0.078_636_05, 1.098_273_6],
        ]),
        9 => Some([
            [1.660_491, -0.587_641_1, -0.072_849_9],
            [-0.124_550_5, 1.132_899_9, -0.008_349_4],
            [-0.018_150_8, -0.100_578_9, 1.118_729_7],
        ]),
        _ => None,
    }
}

/// An alpha picture's luma as one byte a texel, by the range its sequence
/// header declares.
pub(crate) fn alpha_bytes(alpha: &Picture<'_>) -> Result<Vec<u8>, AssetError> {
    let plane = alpha.plane(0)?;
    let full = alpha.signal().is_none_or(|s| s.full);
    let range = Range::new(alpha.depth(), full);
    let (w, h) = alpha.size();
    let mut out = Vec::with_capacity(w * h);
    for y in 0..h {
        for x in 0..w {
            out.push((range.luma(plane.at(x, y)).clamp(0.0, 1.0) * 255.0 + 0.5) as u8);
        }
    }
    Ok(out)
}

/// Converts `color`, with `alpha` — one byte a texel — when the image has
/// an alpha plane, by `signal`, to `width × height` straight RGBA8.
/// `premultiplied` color is divided by its alpha first; `peak` is the
/// brightest the content reaches, in nits, when it records it.
pub(crate) fn to_rgba(
    color: &Picture<'_>,
    alpha: Option<&[u8]>,
    signal: Signal,
    premultiplied: bool,
    peak: Option<f32>,
) -> Result<Vec<u8>, AssetError> {
    let (w, h) = color.size();
    let depth = color.depth();
    let layout = color.layout()?;
    let luma = color.plane(0)?;
    let chroma = match layout {
        Layout::Mono => None,
        _ => Some((color.plane(1)?, color.plane(2)?)),
    };
    let matrix = match (layout, signal.matrix) {
        (Layout::Mono, _) => Matrix::Mono,
        (_, 0) => Matrix::Identity,
        (_, 8) => Matrix::YCgCo,
        (_, 1) => Matrix::YCbCr {
            kr: 0.2126,
            kb: 0.0722,
        },
        (_, 4) => Matrix::YCbCr { kr: 0.30, kb: 0.11 },
        (_, 7) => Matrix::YCbCr {
            kr: 0.212,
            kb: 0.087,
        },
        (_, 9 | 10) => Matrix::YCbCr {
            kr: 0.2627,
            kb: 0.0593,
        },
        // BT.601, and unspecified.
        _ => Matrix::YCbCr {
            kr: 0.299,
            kb: 0.114,
        },
    };
    let range = Range::new(depth, signal.full);
    let alpha = alpha.filter(|a| a.len() == w * h);
    let curve = Curve::of(signal.transfer);
    let gamut = gamut(signal.primaries);
    let hdr = matches!(curve, Curve::Pq | Curve::Hlg);
    let table = (curve != Curve::Srgb || gamut.is_some()).then(|| Table::new(curve));
    let white = match curve {
        Curve::Pq => peak.unwrap_or(HDR_PEAK).clamp(SDR_WHITE, 10_000.0),
        _ => HDR_PEAK,
    } / SDR_WHITE;
    // Where each output column's chroma lies: the site at or before it and
    // the share of the one after.
    let sites = |n: usize, half: bool| -> Vec<(usize, f32)> {
        (0..n)
            .map(|x| {
                if !half {
                    return (x, 0.0);
                }
                let at = (x as f32 * 0.5 - 0.25).max(0.0);
                (at as usize, at.fract())
            })
            .collect()
    };
    let (across, down) = match layout {
        Layout::Half => (true, true),
        Layout::HalfAcross => (true, false),
        _ => (false, false),
    };
    let columns = sites(w, across);
    let rows = sites(h, down);
    let mut out = vec![0u8; w * h * 4];
    for (y, &(cy, ty)) in rows.iter().enumerate() {
        let row = &mut out[y * w * 4..(y + 1) * w * 4];
        for (x, &(cx, tx)) in columns.iter().enumerate() {
            let yv = range.luma(luma.at(x, y));
            let [mut r, mut g, mut b] = match (matrix, &chroma) {
                (Matrix::Mono, _) | (_, None) => [yv; 3],
                (_, Some((u, v))) => {
                    let cb = range.chroma(bilinear(u, cx, cy, tx, ty));
                    let cr = range.chroma(bilinear(v, cx, cy, tx, ty));
                    match matrix {
                        Matrix::YCbCr { kr, kb } => {
                            let r = yv + 2.0 * (1.0 - kr) * cr;
                            let b = yv + 2.0 * (1.0 - kb) * cb;
                            let g = (yv - kr * r - kb * b) / (1.0 - kr - kb);
                            [r, g, b]
                        }
                        // The planes are G, B, R, each at the luma's range.
                        Matrix::Identity => [cr + 0.5, yv, cb + 0.5],
                        Matrix::YCgCo => {
                            let t = yv - cb;
                            [t + cr, yv + cb, t - cr]
                        }
                        Matrix::Mono => [yv; 3],
                    }
                }
            };
            let a = alpha.map_or(255, |a| a[y * w + x]);
            if premultiplied && a != 0 && a != 255 {
                let k = 255.0 / f32::from(a);
                (r, g, b) = (r * k, g * k, b * k);
            }
            let px = &mut row[x * 4..x * 4 + 4];
            px[3] = a;
            let Some(table) = &table else {
                let byte = |v: f32| (v.clamp(0.0, 1.0) * 255.0 + 0.5) as u8;
                px[..3].copy_from_slice(&[byte(r), byte(g), byte(b)]);
                continue;
            };
            let mut lin = [table.at(r), table.at(g), table.at(b)];
            if curve == Curve::Hlg {
                // The display renders scene light at its peak, with the
                // system gamma on luminance.
                let ys = 0.2627 * lin[0] + 0.6780 * lin[1] + 0.0593 * lin[2];
                let scale = ys.max(1e-6).powf(0.2) * white;
                lin = lin.map(|c| c * scale);
            }
            if let Some(m) = &gamut {
                lin = [0, 1, 2].map(|i| m[i][0] * lin[0] + m[i][1] * lin[1] + m[i][2] * lin[2]);
            }
            if hdr {
                // An extended Reinhard on the brightest channel: white at
                // the peak, the hue kept.
                let top = lin[0].max(lin[1]).max(lin[2]);
                if top > 0.0 {
                    let mapped = top * (1.0 + top / (white * white)) / (1.0 + top);
                    lin = lin.map(|c| c * mapped / top);
                }
            }
            px[..3].copy_from_slice(&lin.map(srgb::encode));
        }
    }
    Ok(out)
}

/// The chroma at site `x, y` and `tx`, `ty` of the way to the next.
#[inline(always)]
fn bilinear(p: &Plane<'_>, x: usize, y: usize, tx: f32, ty: f32) -> f32 {
    let s = |x, y| f32::from(p.at(x, y));
    let top = s(x, y) + (s(x + 1, y) - s(x, y)) * tx;
    if ty == 0.0 {
        return top;
    }
    let bottom = s(x, y + 1) + (s(x + 1, y + 1) - s(x, y + 1)) * tx;
    top + (bottom - top) * ty
}

/// How samples of a depth map to `[0, 1]` luma and `[-0.5, 0.5]` chroma.
struct Range {
    luma_offset: f32,
    luma_scale: f32,
    chroma_offset: f32,
    chroma_scale: f32,
}

impl Range {
    fn new(depth: u8, full: bool) -> Range {
        let max = f32::from((1u16 << depth.min(15)).wrapping_sub(1).max(1));
        let unit = f32::from(1u16 << (depth.clamp(8, 15) - 8));
        if full {
            Range {
                luma_offset: 0.0,
                luma_scale: 1.0 / max,
                chroma_offset: (max + 1.0) / 2.0,
                chroma_scale: 1.0 / max,
            }
        } else {
            Range {
                luma_offset: 16.0 * unit,
                luma_scale: 1.0 / (219.0 * unit),
                chroma_offset: 128.0 * unit,
                chroma_scale: 1.0 / (224.0 * unit),
            }
        }
    }

    #[inline(always)]
    fn luma(&self, v: u16) -> f32 {
        (f32::from(v) - self.luma_offset) * self.luma_scale
    }

    #[inline(always)]
    fn chroma(&self, v: f32) -> f32 {
        (v - self.chroma_offset) * self.chroma_scale
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn limited_and_full_range_map_their_ends() {
        let limited = Range::new(8, false);
        assert!((limited.luma(16)).abs() < 1e-6);
        assert!((limited.luma(235) - 1.0).abs() < 1e-6);
        assert!((limited.chroma(128.0)).abs() < 1e-6);
        let full = Range::new(10, true);
        assert!((full.luma(1023) - 1.0).abs() < 1e-6);
        assert!(full.chroma(512.0).abs() < 1e-3);
        let ten = Range::new(10, false);
        assert!((ten.luma(940) - 1.0).abs() < 1e-6);
    }

    #[test]
    fn the_curves_meet_at_their_reference_points() {
        // PQ's code for 203 nits is about 0.58.
        let white = Curve::Pq.eotf(0.580_8);
        assert!((white - 1.0).abs() < 0.01, "{white}");
        assert!((Curve::Hlg.eotf(0.5) - 1.0 / 12.0).abs() < 1e-6);
        assert!((Curve::Srgb.eotf(1.0) - 1.0).abs() < 1e-6);
        let table = Table::new(Curve::Gamma(2.2));
        assert!((table.at(0.5) - 0.5f32.powf(2.2)).abs() < 1e-4);
    }
}
