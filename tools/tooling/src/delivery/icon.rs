//! The app's icon: the one square PNG `[package.icons] source` names, and
//! every size a platform asks for scaled down from it.
//!
//! A size is an area average in linear light over premultiplied pixels, so
//! edges neither darken nor fringe with the color of transparent pixels.

use std::sync::OnceLock;

use viso_image::snapshot::Image;

/// The smallest source: the size the stores show.
pub const MIN_SIZE: u32 = 1024;

/// Reads the icon `bytes`: a square PNG of at least [`MIN_SIZE`] pixels, or
/// why it is not.
pub fn read(bytes: &[u8]) -> Result<Image, String> {
    let image = Image::from_png(bytes).map_err(|why| format!("not a PNG this reads: {why}"))?;
    if image.width != image.height {
        return Err(format!(
            "{}x{} pixels, not square",
            image.width, image.height
        ));
    }
    if image.width < MIN_SIZE {
        return Err(format!(
            "{} pixels across, less than {MIN_SIZE}",
            image.width
        ));
    }
    Ok(image)
}

/// Whether any pixel of `image` is not fully opaque.
pub fn transparent(image: &Image) -> bool {
    image.rgba.as_chunks::<4>().0.iter().any(|px| px[3] != 255)
}

/// `image` over white, every pixel opaque.
pub fn opaque(image: &Image) -> Image {
    let linear = to_linear();
    let rgba = image
        .rgba
        .as_chunks::<4>()
        .0
        .iter()
        .flat_map(|px| {
            let a = f32::from(px[3]) / 255.0;
            let mix = |c: u8| to_srgb(linear[usize::from(c)] * a + (1.0 - a));
            [mix(px[0]), mix(px[1]), mix(px[2]), 255]
        })
        .collect();
    Image {
        width: image.width,
        height: image.height,
        rgba,
    }
}

/// `image`, square, scaled to `size` pixels across; `size` is no larger.
pub fn scaled(image: &Image, size: u32) -> Image {
    if size == image.width {
        return image.clone();
    }
    let linear = to_linear();
    let (from, to) = (image.width as usize, size as usize);
    // Premultiplied linear light, so the average weighs each color by how
    // much of it covers the area.
    let source: Vec<[f32; 4]> = image
        .rgba
        .as_chunks::<4>()
        .0
        .iter()
        .map(|px| {
            let a = f32::from(px[3]) / 255.0;
            [
                linear[usize::from(px[0])] * a,
                linear[usize::from(px[1])] * a,
                linear[usize::from(px[2])] * a,
                a,
            ]
        })
        .collect();
    let weights = coverage(from, to);
    // Rows first, then columns.
    let mut rows = vec![[0f32; 4]; to * from];
    for y in 0..from {
        for (x, taps) in weights.iter().enumerate() {
            let mut sum = [0f32; 4];
            for &(i, w) in taps {
                let px = source[y * from + i];
                for c in 0..4 {
                    sum[c] += px[c] * w;
                }
            }
            rows[y * to + x] = sum;
        }
    }
    let mut rgba = Vec::with_capacity(to * to * 4);
    for taps in &weights {
        for x in 0..to {
            let mut sum = [0f32; 4];
            for &(i, w) in taps {
                let px = rows[i * to + x];
                for c in 0..4 {
                    sum[c] += px[c] * w;
                }
            }
            let a = sum[3];
            if a <= 0.0 {
                rgba.extend_from_slice(&[0, 0, 0, 0]);
            } else {
                rgba.extend_from_slice(&[
                    to_srgb(sum[0] / a),
                    to_srgb(sum[1] / a),
                    to_srgb(sum[2] / a),
                    (a * 255.0).round().clamp(0.0, 255.0) as u8,
                ]);
            }
        }
    }
    Image {
        width: size,
        height: size,
        rgba,
    }
}

/// For each of `to` output pixels, the `from` source pixels it covers and
/// how much of it each covers; each pixel's weights sum to 1.
fn coverage(from: usize, to: usize) -> Vec<Vec<(usize, f32)>> {
    let scale = from as f64 / to as f64;
    (0..to)
        .map(|x| {
            let start = x as f64 * scale;
            let end = start + scale;
            let first = start.floor() as usize;
            let last = (end.ceil() as usize).min(from);
            (first..last)
                .map(|i| {
                    let covered = (end.min(i as f64 + 1.0) - start.max(i as f64)) / scale;
                    (i, covered as f32)
                })
                .filter(|&(_, w)| w > 0.0)
                .collect()
        })
        .collect()
}

/// sRGB bytes to linear light.
fn to_linear() -> &'static [f32; 256] {
    static TABLE: OnceLock<[f32; 256]> = OnceLock::new();
    TABLE.get_or_init(|| {
        std::array::from_fn(|i| {
            let c = i as f32 / 255.0;
            if c <= 0.040_45 {
                c / 12.92
            } else {
                ((c + 0.055) / 1.055).powf(2.4)
            }
        })
    })
}

/// Linear light to an sRGB byte.
fn to_srgb(linear: f32) -> u8 {
    let c = linear.clamp(0.0, 1.0);
    let s = if c <= 0.003_130_8 {
        c * 12.92
    } else {
        1.055 * c.powf(1.0 / 2.4) - 0.055
    };
    (s * 255.0).round() as u8
}

#[cfg(test)]
mod tests {
    use super::*;

    fn filled(size: u32, px: [u8; 4]) -> Image {
        Image {
            width: size,
            height: size,
            rgba: px.repeat((size * size) as usize),
        }
    }

    #[test]
    fn a_source_is_a_square_png_of_the_stores_size() {
        assert!(read(b"not a png").unwrap_err().starts_with("not a PNG"));
        let wide = Image {
            width: 1024,
            height: 512,
            rgba: vec![0; 1024 * 512 * 4],
        };
        assert_eq!(
            read(&wide.to_png()).unwrap_err(),
            "1024x512 pixels, not square"
        );
        assert!(
            read(&filled(512, [0; 4]).to_png())
                .unwrap_err()
                .contains("less than 1024")
        );
        assert_eq!(
            read(&filled(1024, [9, 9, 9, 255]).to_png()).unwrap().width,
            1024
        );
    }

    #[test]
    fn a_flat_color_scales_to_itself() {
        let image = filled(1024, [200, 30, 90, 255]);
        for size in [16, 48, 72, 1000] {
            let small = scaled(&image, size);
            assert_eq!(small.width, size);
            assert!(
                small
                    .rgba
                    .as_chunks::<4>()
                    .0
                    .iter()
                    .all(|px| *px == [200, 30, 90, 255]),
                "{size}"
            );
        }
    }

    /// Half black and half white averages to the middle of the light, not
    /// the middle of the bytes; a transparent half adds no color.
    #[test]
    fn halves_average_in_linear_light() {
        let mut image = filled(4, [0, 0, 0, 255]);
        for y in 0..4 {
            for x in 2..4 {
                let at = (y * 4 + x) * 4;
                image.rgba[at..at + 4].copy_from_slice(&[255, 255, 255, 255]);
            }
        }
        assert_eq!(scaled(&image, 1).rgba, [188, 188, 188, 255]);

        let mut image = filled(2, [0, 0, 0, 0]);
        image.rgba[..4].copy_from_slice(&[255, 0, 0, 255]);
        assert_eq!(scaled(&image, 1).rgba, [255, 0, 0, 64]);
        assert!(transparent(&image));
        assert_eq!(&opaque(&image).rgba[4..8], [255, 255, 255, 255]);
    }

    #[test]
    fn weights_cover_each_pixel_once() {
        for (from, to) in [(1024, 48), (1024, 1000), (7, 3)] {
            for taps in coverage(from, to) {
                let sum: f32 = taps.iter().map(|(_, w)| w).sum();
                assert!((sum - 1.0).abs() < 1e-4, "{from}->{to}: {sum}");
            }
        }
    }
}
