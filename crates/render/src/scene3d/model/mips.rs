//! Mip chains built at load: each level a 2×2 box filter of the one above,
//! in the space its texels mean something in.

use viso_gpu::srgb;

/// What an image's texels hold, which decides how its levels are averaged.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ImageKind {
    /// sRGB-encoded color with straight alpha: averaged in linear light,
    /// weighted by alpha so a transparent texel's color does not bleed.
    #[default]
    Color,
    /// Linear data (metalness, roughness, occlusion): averaged as is.
    Data,
    /// A tangent-space normal map: averaged as vectors and renormalized.
    Normal,
}

/// The levels below `rgba`, a `width × height` straight RGBA8 image, down
/// to 1×1: each half the previous in each dimension, rounded down, at least
/// one texel.
pub(crate) fn chain(rgba: &[u8], width: u32, height: u32, kind: ImageKind) -> Vec<Vec<u8>> {
    let mut levels: Vec<Vec<u8>> = Vec::new();
    let (mut w, mut h) = (width as usize, height as usize);
    if rgba.len() < w * h * 4 {
        return levels;
    }
    while w > 1 || h > 1 {
        let (nw, nh) = ((w / 2).max(1), (h / 2).max(1));
        let above: &[u8] = levels.last().map_or(rgba, Vec::as_slice);
        let mut out = vec![0u8; nw * nh * 4];
        for y in 0..nh {
            for x in 0..nw {
                // The 2×2 block above, collapsed along an axis of one texel.
                let xs = [(2 * x).min(w - 1), (2 * x + 1).min(w - 1)];
                let ys = [(2 * y).min(h - 1), (2 * y + 1).min(h - 1)];
                let texel = |xx: usize, yy: usize| {
                    let i = (yy * w + xx) * 4;
                    [above[i], above[i + 1], above[i + 2], above[i + 3]]
                };
                let block = [
                    texel(xs[0], ys[0]),
                    texel(xs[1], ys[0]),
                    texel(xs[0], ys[1]),
                    texel(xs[1], ys[1]),
                ];
                let px = average(&block, kind);
                out[(y * nw + x) * 4..(y * nw + x) * 4 + 4].copy_from_slice(&px);
            }
        }
        levels.push(out);
        (w, h) = (nw, nh);
    }
    levels
}

fn average(block: &[[u8; 4]; 4], kind: ImageKind) -> [u8; 4] {
    let unorm = |v: f32| (v.clamp(0.0, 1.0) * 255.0 + 0.5) as u8;
    match kind {
        ImageKind::Data => core::array::from_fn(|c| {
            let sum: u32 = block.iter().map(|t| u32::from(t[c])).sum();
            ((sum + 2) / 4) as u8
        }),
        ImageKind::Color => {
            let mut rgb = [0.0f32; 3];
            let mut alpha = 0.0f32;
            for t in block {
                let a = f32::from(t[3]) / 255.0;
                for c in 0..3 {
                    rgb[c] += srgb::decode(t[c]) * a;
                }
                alpha += a;
            }
            if alpha <= 0.0 {
                // Fully transparent: the plain average keeps a color for the
                // filter to blend toward.
                let mut plain = [0.0f32; 3];
                for t in block {
                    for c in 0..3 {
                        plain[c] += srgb::decode(t[c]) * 0.25;
                    }
                }
                return [
                    srgb::encode(plain[0]),
                    srgb::encode(plain[1]),
                    srgb::encode(plain[2]),
                    0,
                ];
            }
            [
                srgb::encode(rgb[0] / alpha),
                srgb::encode(rgb[1] / alpha),
                srgb::encode(rgb[2] / alpha),
                unorm(alpha * 0.25),
            ]
        }
        ImageKind::Normal => {
            let mut n = [0.0f32; 3];
            for t in block {
                for c in 0..3 {
                    n[c] += f32::from(t[c]) / 255.0 * 2.0 - 1.0;
                }
            }
            let len = (n[0] * n[0] + n[1] * n[1] + n[2] * n[2]).sqrt();
            let n = if len > 1e-6 {
                n.map(|v| v / len)
            } else {
                [0.0, 0.0, 1.0]
            };
            let a: u32 = block.iter().map(|t| u32::from(t[3])).sum();
            [
                unorm(n[0] * 0.5 + 0.5),
                unorm(n[1] * 0.5 + 0.5),
                unorm(n[2] * 0.5 + 0.5),
                ((a + 2) / 4) as u8,
            ]
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_chain_halves_down_to_one_texel() {
        let rgba = vec![255u8; 8 * 2 * 4];
        let levels = chain(&rgba, 8, 2, ImageKind::Data);
        let sizes: Vec<usize> = levels.iter().map(|l| l.len() / 4).collect();
        assert_eq!(sizes, [4, 2, 1]);
    }

    #[test]
    fn color_averages_in_linear_light_and_ignores_transparent_texels() {
        // Black and white average to mid grey in light, not 128.
        let rgba = [0, 0, 0, 255, 255, 255, 255, 255];
        let level = &chain(&rgba, 2, 1, ImageKind::Color)[0];
        assert!((186..=189).contains(&level[0]), "{level:?}");
        // A transparent red texel beside an opaque blue one leaves blue.
        let rgba = [255, 0, 0, 0, 0, 0, 255, 255];
        let level = &chain(&rgba, 2, 1, ImageKind::Color)[0];
        assert_eq!(&level[..3], &[0, 0, 255]);
        assert_eq!(level[3], 128);
    }

    #[test]
    fn normals_average_to_unit_vectors() {
        // Two normals tilted opposite ways average to straight up.
        let rgba = [218, 128, 218, 255, 37, 128, 218, 255];
        let level = &chain(&rgba, 2, 1, ImageKind::Normal)[0];
        assert!(
            (126..=129).contains(&level[0]) && level[2] == 255,
            "{level:?}"
        );
    }
}
