//! Area resampling of an A8 bitmap by a rational scale, in integers only.
//!
//! A build that bakes its glyphs and links no rasterizer draws a size it did
//! not bake from the nearest size it did. Each destination pixel is the
//! average coverage of the source area under it: with the scale `to / from`,
//! source pixel `i` spans `[i·to, (i+1)·to)` and destination pixel `x` spans
//! `[x·from, (x+1)·from)` on one integer axis, so every overlap is an exact
//! integer and so is the weighted sum.

use alloc::vec;
use alloc::vec::Vec;

use crate::CoverageBitmap;

/// `bitmap`, drawn at `from` pixels per em, scaled to `to` pixels per em.
pub(crate) fn resample(bitmap: &CoverageBitmap, from: u16, to: u16) -> CoverageBitmap {
    let (from, to) = (u64::from(from.max(1)), u64::from(to.max(1)));
    let scale = to as f32 / from as f32;
    let (left, top) = (bitmap.left * scale, bitmap.top * scale);
    if bitmap.is_empty() {
        return CoverageBitmap::empty(left, top);
    }
    let width = scaled(bitmap.width, from, to);
    let height = scaled(bitmap.height, from, to);
    let (sw, sh) = (bitmap.width as usize, bitmap.height as usize);
    let (dw, dh) = (width as usize, height as usize);

    // Horizontal pass: each row's sums over the destination columns, in units
    // of `from` source-pixel widths.
    let mut rows = vec![0u64; dw * sh];
    for y in 0..sh {
        let source = &bitmap.coverage[y * sw..(y + 1) * sw];
        let row = &mut rows[y * dw..(y + 1) * dw];
        for (x, sum) in row.iter_mut().enumerate() {
            *sum = footprint(x as u64, sw, from, to, |i| u64::from(source[i]));
        }
    }
    // Vertical pass over the row sums; the area is `from × from`.
    let area = from * from;
    let mut coverage = Vec::with_capacity(dw * dh);
    for y in 0..dh {
        for x in 0..dw {
            let sum = footprint(y as u64, sh, from, to, |i| rows[i * dw + x]);
            coverage.push(((sum + area / 2) / area).min(255) as u8);
        }
    }
    CoverageBitmap {
        width,
        height,
        left,
        top,
        coverage,
    }
}

/// `len` source pixels at the scale `to / from`, rounded up to whole pixels.
fn scaled(len: u32, from: u64, to: u64) -> u32 {
    (u64::from(len) * to).div_ceil(from).max(1) as u32
}

/// The sum of `value(i)` over the source pixels under destination pixel `x`,
/// each weighted by its overlap; pixels past `len` are empty.
fn footprint(x: u64, len: usize, from: u64, to: u64, value: impl Fn(usize) -> u64) -> u64 {
    let (start, end) = (x * from, (x + 1) * from);
    let first = (start / to) as usize;
    let last = (end.div_ceil(to) as usize).min(len);
    let mut sum = 0;
    for i in first..last {
        let (lo, hi) = (i as u64 * to, (i as u64 + 1) * to);
        let overlap = end.min(hi) - start.max(lo);
        sum += value(i) * overlap;
    }
    sum
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bitmap(width: u32, height: u32, coverage: Vec<u8>) -> CoverageBitmap {
        CoverageBitmap {
            width,
            height,
            left: 2.0,
            top: 8.0,
            coverage,
        }
    }

    #[test]
    fn the_same_size_is_the_same_bitmap() {
        let source = bitmap(3, 2, vec![0, 128, 255, 10, 20, 30]);
        assert_eq!(resample(&source, 16, 16), source);
    }

    #[test]
    fn halving_averages_each_two_by_two_block() {
        let source = bitmap(4, 2, vec![0, 255, 255, 255, 255, 0, 255, 255]);
        let half = resample(&source, 32, 16);
        assert_eq!((half.width, half.height), (2, 1));
        assert_eq!(half.coverage, [128, 255]);
        assert_eq!((half.left, half.top), (1.0, 4.0));
    }

    #[test]
    fn doubling_repeats_each_pixel_and_keeps_solid_solid() {
        let source = bitmap(2, 1, vec![255, 0]);
        let double = resample(&source, 8, 16);
        assert_eq!((double.width, double.height), (4, 2));
        assert_eq!(double.coverage, [255, 255, 0, 0, 255, 255, 0, 0]);
    }

    #[test]
    fn a_partial_last_pixel_counts_the_area_past_the_edge_as_empty() {
        // 3 px at 2/3 scale is 2 px: the second covers source 1.5..3.
        let source = bitmap(3, 3, vec![255; 9]);
        let scaled = resample(&source, 3, 2);
        assert_eq!((scaled.width, scaled.height), (2, 2));
        assert_eq!(scaled.coverage, [255; 4]);
        // One solid row at that scale covers two thirds of its pixel.
        let row = resample(&bitmap(3, 1, vec![255; 3]), 3, 2);
        assert_eq!(row.coverage, [170, 170]);
        let source = bitmap(1, 1, vec![255]);
        let scaled = resample(&source, 2, 3);
        assert_eq!((scaled.width, scaled.height), (2, 2));
        assert_eq!(scaled.coverage, [255, 128, 128, 64]);
    }
}
