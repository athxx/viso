//! Texel codecs: an image of premultiplied RGBA8 encoded into the format a
//! target samples, and decoded back.
//!
//! - [`packed`]: the 16-bit formats (`Rgb565Unorm`, `Rgba4444Unorm`,
//!   `Rgba5551Unorm`), quantized through an ordered dither so smooth ramps do
//!   not band.
//! - [`bc`]: BC1, BC3 and BC7, the desktop GPUs' formats.
//! - [`etc`]: ETC2 RGB, RGB with punch-through alpha, and RGBA (EAC alpha),
//!   mandatory on OpenGL ES 3.
//! - [`astc`]: ASTC LDR at 4×4, 6×6 and 8×8 texel blocks, the mobile GPUs'.
//!
//! Each block codec works on one block of texels, row by row from its top
//! left; [`encode`] and [`decode`] walk an image's blocks, padding a partial
//! block at the right or bottom edge by repeating the edge texels. Encoding
//! runs at build time and spreads block rows over the machine's threads;
//! decoding is the runtime's fallback for a device that does not sample the
//! format, and the reference the tests compare a device's sampling against.
//! Every texel is premultiplied: the encoders compress premultiplied color, so
//! filtering a decoded texture never bleeds the color of transparent texels.

pub mod astc;
pub mod bc;
pub mod etc;
pub mod packed;

use viso_gpu_abi::TextureFormat;

/// How hard an encoder searches for the block that decodes closest to its
/// texels. The output is the same format either way; `Thorough` is what a
/// release build spends, `Fast` what a test or a quick preview does.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Effort {
    Fast,
    #[default]
    Thorough,
}

/// Whether this crate encodes and decodes `format`.
pub const fn supports(format: TextureFormat) -> bool {
    !matches!(
        format,
        TextureFormat::R8Unorm
            | TextureFormat::Rgba8Data
            | TextureFormat::Rgba16Float
            | TextureFormat::Depth32Float
    )
}

/// `texels`, `width × height` premultiplied RGBA8 row by row from the top,
/// encoded as `format`: [`TextureFormat::data_size`] bytes, rows of blocks
/// top first.
///
/// # Panics
///
/// When `texels` does not hold `width × height` texels, or `format` is not
/// one [`supports`] names.
pub fn encode(
    format: TextureFormat,
    width: u32,
    height: u32,
    texels: &[[u8; 4]],
    effort: Effort,
) -> Vec<u8> {
    assert_eq!(
        texels.len(),
        width as usize * height as usize,
        "texel count"
    );
    assert!(supports(format), "no encoder for {format:?}");
    match format {
        TextureFormat::Rgba8Unorm => return texels.as_flattened().to_vec(),
        TextureFormat::Bgra8Unorm => {
            return texels
                .iter()
                .flat_map(|&[r, g, b, a]| [b, g, r, a])
                .collect();
        }
        TextureFormat::Rgb565Unorm
        | TextureFormat::Rgba4444Unorm
        | TextureFormat::Rgba5551Unorm => return packed::encode(format, width, texels),
        _ => {}
    }
    let block = format.block();
    let (bw, bh) = (block.width as usize, block.height as usize);
    let (w, h) = (width as usize, height as usize);
    let blocks_x = w.div_ceil(bw);
    let row_bytes = format.row_bytes(width);
    let mut out = vec![0u8; format.data_size(width, height)];
    let encode_row = |by: usize, row: &mut [u8]| {
        let mut texel_block = vec![[0u8; 4]; bw * bh];
        for bx in 0..blocks_x {
            for y in 0..bh {
                let sy = (by * bh + y).min(h - 1);
                for x in 0..bw {
                    let sx = (bx * bw + x).min(w - 1);
                    texel_block[y * bw + x] = texels[sy * w + sx];
                }
            }
            let at = bx * block.bytes as usize;
            encode_block(
                format,
                &texel_block,
                effort,
                &mut row[at..at + block.bytes as usize],
            );
        }
    };
    let threads = std::thread::available_parallelism().map_or(1, usize::from);
    let rows: Vec<(usize, &mut [u8])> = out.chunks_mut(row_bytes).enumerate().collect();
    if threads <= 1 || rows.len() <= 1 {
        for (by, row) in rows {
            encode_row(by, row);
        }
    } else {
        let per = rows.len().div_ceil(threads);
        let mut rows = rows;
        std::thread::scope(|scope| {
            while !rows.is_empty() {
                let take = rows.len().min(per);
                let batch: Vec<(usize, &mut [u8])> = rows.drain(..take).collect();
                let encode_row = &encode_row;
                scope.spawn(move || {
                    for (by, row) in batch {
                        encode_row(by, row);
                    }
                });
            }
        });
    }
    out
}

/// One block of `format` from `texels` (`block.width × block.height`, row by
/// row) into `out` (`block.bytes` long).
fn encode_block(format: TextureFormat, texels: &[[u8; 4]], effort: Effort, out: &mut [u8]) {
    let four = |texels: &[[u8; 4]]| -> [[u8; 4]; 16] { texels.try_into().expect("4×4 texels") };
    match format {
        TextureFormat::Bc1RgbaUnorm => out.copy_from_slice(&bc::encode_bc1(&four(texels), effort)),
        TextureFormat::Bc3RgbaUnorm => out.copy_from_slice(&bc::encode_bc3(&four(texels), effort)),
        TextureFormat::Bc7RgbaUnorm => out.copy_from_slice(&bc::encode_bc7(&four(texels), effort)),
        TextureFormat::Etc2Rgb8Unorm => {
            out.copy_from_slice(&etc::encode_etc2_rgb(&four(texels), effort));
        }
        TextureFormat::Etc2Rgb8A1Unorm => {
            out.copy_from_slice(&etc::encode_etc2_rgb8a1(&four(texels), effort));
        }
        TextureFormat::Etc2Rgba8Unorm => {
            out.copy_from_slice(&etc::encode_etc2_rgba8(&four(texels), effort));
        }
        TextureFormat::Astc4x4Unorm | TextureFormat::Astc6x6Unorm | TextureFormat::Astc8x8Unorm => {
            let block = format.block();
            out.copy_from_slice(&astc::encode_astc(
                texels,
                (block.width, block.height),
                effort,
            ));
        }
        other => unreachable!("{other:?} is not a block format"),
    }
}

/// `data`, `width × height` texels of `format` as [`encode`] lays them out,
/// decoded to premultiplied RGBA8 row by row from the top; `None` when it is
/// not [`TextureFormat::data_size`] bytes or the format is not one
/// [`supports`] names.
pub fn decode(format: TextureFormat, width: u32, height: u32, data: &[u8]) -> Option<Vec<[u8; 4]>> {
    if !supports(format) || data.len() != format.data_size(width, height) {
        return None;
    }
    let (w, h) = (width as usize, height as usize);
    match format {
        TextureFormat::Rgba8Unorm => return Some(data.as_chunks::<4>().0.to_vec()),
        TextureFormat::Bgra8Unorm => {
            return Some(
                data.as_chunks::<4>()
                    .0
                    .iter()
                    .map(|&[b, g, r, a]| [r, g, b, a])
                    .collect(),
            );
        }
        TextureFormat::Rgb565Unorm
        | TextureFormat::Rgba4444Unorm
        | TextureFormat::Rgba5551Unorm => return Some(packed::decode(format, data)),
        _ => {}
    }
    let block = format.block();
    let (bw, bh) = (block.width as usize, block.height as usize);
    let mut out = vec![[0u8; 4]; w * h];
    let mut texels = vec![[0u8; 4]; bw * bh];
    for (i, bytes) in data.chunks_exact(block.bytes as usize).enumerate() {
        let (bx, by) = (i % w.div_ceil(bw), i / w.div_ceil(bw));
        decode_block(format, bytes, &mut texels);
        for y in 0..bh {
            let ty = by * bh + y;
            if ty >= h {
                break;
            }
            for x in 0..bw {
                let tx = bx * bw + x;
                if tx < w {
                    out[ty * w + tx] = texels[y * bw + x];
                }
            }
        }
    }
    Some(out)
}

/// One block of `format` decoded into `out` (`block.width × block.height`
/// texels, row by row).
fn decode_block(format: TextureFormat, bytes: &[u8], out: &mut [[u8; 4]]) {
    let eight = |bytes: &[u8]| -> [u8; 8] { bytes.try_into().expect("an 8-byte block") };
    let sixteen = |bytes: &[u8]| -> [u8; 16] { bytes.try_into().expect("a 16-byte block") };
    match format {
        TextureFormat::Bc1RgbaUnorm => out.copy_from_slice(&bc::decode_bc1(&eight(bytes))),
        TextureFormat::Bc3RgbaUnorm => out.copy_from_slice(&bc::decode_bc3(&sixteen(bytes))),
        TextureFormat::Bc7RgbaUnorm => out.copy_from_slice(&bc::decode_bc7(&sixteen(bytes))),
        TextureFormat::Etc2Rgb8Unorm => out.copy_from_slice(&etc::decode_etc2_rgb(&eight(bytes))),
        TextureFormat::Etc2Rgb8A1Unorm => {
            out.copy_from_slice(&etc::decode_etc2_rgb8a1(&eight(bytes)));
        }
        TextureFormat::Etc2Rgba8Unorm => {
            out.copy_from_slice(&etc::decode_etc2_rgba8(&sixteen(bytes)));
        }
        TextureFormat::Astc4x4Unorm | TextureFormat::Astc6x6Unorm | TextureFormat::Astc8x8Unorm => {
            let block = format.block();
            astc::decode_astc(&sixteen(bytes), (block.width, block.height), out);
        }
        other => unreachable!("{other:?} is not a block format"),
    }
}

/// Peak signal-to-noise ratio of `b` against `a` over the color channels of
/// texels visible in either (alpha weighted in), in dB; infinite when equal.
/// The figure the codec tests hold each encoder to.
pub fn psnr(a: &[[u8; 4]], b: &[[u8; 4]]) -> f64 {
    assert_eq!(a.len(), b.len(), "texel count");
    let mut sum = 0.0f64;
    for (p, q) in a.iter().zip(b) {
        for c in 0..4 {
            let d = f64::from(p[c]) - f64::from(q[c]);
            sum += d * d;
        }
    }
    let mse = sum / (a.len() as f64 * 4.0);
    if mse == 0.0 {
        f64::INFINITY
    } else {
        10.0 * (255.0f64 * 255.0 / mse).log10()
    }
}

#[cfg(test)]
pub(crate) mod fixtures {
    //! Test images every codec is measured on: deterministic, so a threshold
    //! a codec meets once it meets always.

    /// A `w × h` opaque "photo": smooth gradients under low-amplitude noise,
    /// with a few hard edges.
    pub(crate) fn photo(w: u32, h: u32) -> Vec<[u8; 4]> {
        let mut seed = 0x2545_f491_4f6c_dd1du64;
        let mut noise = move || {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            (seed % 9) as i32 - 4
        };
        let mut out = Vec::with_capacity((w * h) as usize);
        for y in 0..h {
            for x in 0..w {
                let (fx, fy) = (x as f32 / w as f32, y as f32 / h as f32);
                let mut r = 40.0 + 180.0 * fx;
                let mut g = 60.0 + 150.0 * (fy * 3.1).sin().abs();
                let mut b = 200.0 - 150.0 * fx * fy;
                if (x / 13 + y / 17) % 5 == 0 {
                    r = 250.0 - r * 0.3;
                    g *= 0.4;
                    b = 30.0;
                }
                let mut c = |v: f32| (v as i32 + noise()).clamp(0, 255) as u8;
                out.push([c(r), c(g), c(b), 255]);
            }
        }
        out
    }

    /// A `w × h` icon, premultiplied: a soft-edged disc and a hard-edged bar
    /// over transparency.
    pub(crate) fn icon(w: u32, h: u32) -> Vec<[u8; 4]> {
        let mut out = Vec::with_capacity((w * h) as usize);
        let (cx, cy, r) = (w as f32 * 0.4, h as f32 * 0.45, w.min(h) as f32 * 0.3);
        for y in 0..h {
            for x in 0..w {
                let (px, py) = (x as f32 + 0.5, y as f32 + 0.5);
                let d = ((px - cx).powi(2) + (py - cy).powi(2)).sqrt();
                let disc = (r - d + 1.5).clamp(0.0, 3.0) / 3.0;
                let bar = f32::from(x > w * 3 / 4 && y > h / 5 && y < h * 4 / 5);
                let a = disc.max(bar);
                let color = if bar > 0.0 {
                    [20.0, 200.0, 90.0]
                } else {
                    [230.0, 120.0 + 100.0 * py / h as f32, 40.0]
                };
                let p = |c: f32| (c * a + 0.5) as u8;
                out.push([
                    p(color[0]),
                    p(color[1]),
                    p(color[2]),
                    (a * 255.0 + 0.5) as u8,
                ]);
            }
        }
        out
    }

    /// `texels` with every alpha snapped to 0 or 255, color cleared where
    /// transparent: the binary-alpha input the punch-through formats take.
    pub(crate) fn binary(texels: &[[u8; 4]]) -> Vec<[u8; 4]> {
        texels
            .iter()
            .map(|&[r, g, b, a]| {
                if a >= 128 {
                    let un = |c: u8| (u32::from(c) * 255 / u32::from(a).max(1)).min(255) as u8;
                    [un(r), un(g), un(b), 255]
                } else {
                    [0; 4]
                }
            })
            .collect()
    }
}
