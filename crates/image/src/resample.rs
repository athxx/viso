//! An image resized on the CPU: scaled down to the most texels a display can
//! show, or stretched to whole blocks of a compressed format.
//!
//! Separable Mitchell–Netravali filtering (B = C = 1/3), its support widened
//! by the scale when shrinking so every source texel contributes, over
//! premultiplied texels in linear light — so transparent texels never bleed
//! their color and a fine pattern keeps its brightness — and clamped back to
//! premultiplied range.

use viso_gpu_abi::srgb;

/// The filter's support radius at scale 1.
const RADIUS: f32 = 2.0;

/// The Mitchell–Netravali cubic with B = C = 1/3.
fn mitchell(x: f32) -> f32 {
    const B: f32 = 1.0 / 3.0;
    const C: f32 = 1.0 / 3.0;
    let x = x.abs();
    if x < 1.0 {
        ((12.0 - 9.0 * B - 6.0 * C) * x * x * x
            + (-18.0 + 12.0 * B + 6.0 * C) * x * x
            + (6.0 - 2.0 * B))
            / 6.0
    } else if x < 2.0 {
        ((-B - 6.0 * C) * x * x * x
            + (6.0 * B + 30.0 * C) * x * x
            + (-12.0 * B - 48.0 * C) * x
            + (8.0 * B + 24.0 * C))
            / 6.0
    } else {
        0.0
    }
}

/// For each of `to` output positions along an axis of `from` source texels,
/// the first source texel it reads and the normalized weights from there.
fn weights(from: u32, to: u32) -> Vec<(usize, Vec<f32>)> {
    let scale = to as f32 / from as f32;
    let stretch = (1.0 / scale).max(1.0);
    let radius = RADIUS * stretch;
    (0..to)
        .map(|o| {
            let center = (o as f32 + 0.5) / scale;
            let first = (center - radius).floor().max(0.0) as usize;
            let last = ((center + radius).ceil() as usize).min(from as usize);
            let mut ws: Vec<f32> = (first..last)
                .map(|i| mitchell((i as f32 + 0.5 - center) / stretch))
                .collect();
            let sum: f32 = ws.iter().sum();
            if sum.abs() > f32::EPSILON {
                for w in &mut ws {
                    *w /= sum;
                }
            }
            (first, ws)
        })
        .collect()
}

/// `texels`, a `width × height` premultiplied RGBA8 image, resized to
/// `to_width × to_height`.
///
/// # Panics
///
/// When `texels` does not hold `width × height` texels or a size is zero.
pub fn resize(
    texels: &[[u8; 4]],
    width: u32,
    height: u32,
    to_width: u32,
    to_height: u32,
) -> Vec<[u8; 4]> {
    assert_eq!(
        texels.len(),
        width as usize * height as usize,
        "texel count"
    );
    assert!(
        width > 0 && height > 0 && to_width > 0 && to_height > 0,
        "a size of no texels"
    );
    if (width, height) == (to_width, to_height) {
        return texels.to_vec();
    }
    let (w, tw, th) = (width as usize, to_width as usize, to_height as usize);
    // Rows first: `height × to_width` texels in f32.
    let across = weights(width, to_width);
    let mut wide = vec![[0f32; 4]; height as usize * tw];
    for y in 0..height as usize {
        let row = &texels[y * w..(y + 1) * w];
        for (x, (first, ws)) in across.iter().enumerate() {
            let mut sum = [0f32; 4];
            for (k, wt) in ws.iter().enumerate() {
                let t = linear(row[first + k]);
                for c in 0..4 {
                    sum[c] += t[c] * wt;
                }
            }
            wide[y * tw + x] = sum;
        }
    }
    let down = weights(height, to_height);
    let mut out = vec![[0u8; 4]; tw * th];
    for (y, (first, ws)) in down.iter().enumerate() {
        for x in 0..tw {
            let mut sum = [0f32; 4];
            for (k, wt) in ws.iter().enumerate() {
                let t = wide[(first + k) * tw + x];
                for c in 0..4 {
                    sum[c] += t[c] * wt;
                }
            }
            let a = (sum[3] * 255.0).round().clamp(0.0, 255.0);
            let px = |v: f32| srgb::encode(v.clamp(0.0, a / 255.0));
            out[y * tw + x] = [px(sum[0]), px(sum[1]), px(sum[2]), a as u8];
        }
    }
    out
}

/// A premultiplied sRGB texel in linear light.
fn linear([r, g, b, a]: [u8; 4]) -> [f32; 4] {
    [
        srgb::decode(r),
        srgb::decode(g),
        srgb::decode(b),
        f32::from(a) / 255.0,
    ]
}

/// The texel size a `width × height` image needs on a surface of `surface`
/// physical pixels: unchanged when it fits however it is placed, else scaled
/// down — keeping its aspect — to the most texels a fit into the surface
/// shows (a cover fit, the largest).
pub fn fit_to_surface(width: u32, height: u32, surface: [u32; 2]) -> [u32; 2] {
    let (w, h) = (u64::from(width), u64::from(height));
    let (sw, sh) = (u64::from(surface[0]), u64::from(surface[1]));
    // The cover scale is the larger of sw / w and sh / h; it is at least 1
    // when either side of the surface reaches past the image's.
    if sw >= w || sh >= h {
        return [width, height];
    }
    let [w, h] = if sw * h >= sh * w {
        [sw, (h * sw).div_ceil(w)]
    } else {
        [(w * sh).div_ceil(h), sh]
    };
    [w.max(1) as u32, h.max(1) as u32]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_flat_image_stays_flat_at_any_size() {
        let flat = vec![[100, 50, 25, 200]; 7 * 5];
        for (tw, th) in [(3, 2), (14, 10), (8, 8), (1, 1)] {
            let out = resize(&flat, 7, 5, tw, th);
            assert_eq!(out.len(), (tw * th) as usize);
            assert!(
                out.iter().all(|p| *p == [100, 50, 25, 200]),
                "{tw}×{th}: {:?}",
                out[0]
            );
        }
    }

    #[test]
    fn shrinking_averages_and_keeps_premultiplication() {
        // A checkerboard of white and transparent averages to half-white:
        // half the light, which sRGB stores as 188.
        let checker: Vec<[u8; 4]> = (0..64)
            .map(|i| {
                if (i % 8 + i / 8) % 2 == 0 {
                    [255; 4]
                } else {
                    [0; 4]
                }
            })
            .collect();
        let out = resize(&checker, 8, 8, 2, 2);
        for p in &out {
            assert!((i32::from(p[3]) - 128).abs() <= 3, "{p:?}");
            assert!((i32::from(p[0]) - 188).abs() <= 3, "{p:?}");
            let a = f32::from(p[3]) / 255.0;
            assert!(
                p[..3].iter().all(|&c| srgb::decode(c) <= a + 0.005),
                "{p:?}"
            );
        }
    }

    #[test]
    fn an_image_shrinks_to_what_a_cover_fit_shows() {
        assert_eq!(fit_to_surface(400, 300, [800, 480]), [400, 300]);
        assert_eq!(fit_to_surface(4000, 3000, [800, 480]), [800, 600]);
        // A banner covering the height needs its full width.
        assert_eq!(fit_to_surface(3000, 300, [800, 480]), [3000, 300]);
        assert_eq!(fit_to_surface(3000, 3000, [480, 800]), [800, 800]);
    }
}
