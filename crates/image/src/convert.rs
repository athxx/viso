//! An asset converted at build time into the format its target draws fastest
//! and stores smallest, as a converted image ([`crate::texture`]).
//!
//! The target decides the family: the CPU raster samples raw texels in place
//! (RGBA8, or 16-bit when the image allows it), a GPU samples one compressed
//! family (BC on desktop GPUs, ETC2 on OpenGL ES 3, ASTC on mobile and Apple
//! GPUs). Within the family, the image's alpha — none, one bit, or soft — and
//! the [`Quality`] pick the format:
//!
//! | target | quality | opaque | 1-bit alpha | soft alpha |
//! |---|---|---|---|---|
//! | any | `Lossless` | RGBA8 | RGBA8 | RGBA8 |
//! | CPU, 8-bit panel | `High` | RGBA8 | RGBA8 | RGBA8 |
//! | CPU, RGB565 panel, or `Balanced` | `High`, `Balanced` | RGB565 | RGBA5551 | RGBA8 |
//! | CPU | `Compact` | RGB565 | RGBA5551 | RGBA4444 |
//! | BC | `High` | BC7 | BC7 | BC7 |
//! | BC | `Balanced`, `Compact` | BC1 | BC1 | BC7 |
//! | ETC2 | any lossy | ETC2 RGB | ETC2 RGB8A1 | ETC2 RGBA |
//! | ASTC | `High` / `Balanced` / `Compact` | 4×4 / 6×6 / 8×8 | same | same |
//!
//! Before encoding, an image larger than the surface it shows on is scaled
//! down to the most texels a fit into the surface shows, and a
//! block-compressed one is stretched to whole blocks; both keep the size it
//! lays out at, so conversion never moves layout.

use viso_gpu_abi::TextureFormat;

use crate::codec::{self, Effort};
use crate::raster::{AssetError, RasterImage, Texels};
use crate::resample;

/// How much of an image's look a build may trade for bytes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Quality {
    /// Every texel as decoded: RGBA8.
    Lossless,
    /// Compression that keeps fine detail: BC7, ASTC 4×4, ETC2; 16-bit only
    /// where the panel is 16-bit anyway.
    #[default]
    High,
    /// Smaller where it shows least: BC1 for opaque images, ASTC 6×6, 16-bit
    /// texels on the CPU.
    Balanced,
    /// The smallest: ASTC 8×8, 4-bit alpha on the CPU.
    Compact,
}

impl Quality {
    /// The name a manifest gives it.
    pub const fn as_str(self) -> &'static str {
        match self {
            Quality::Lossless => "lossless",
            Quality::High => "high",
            Quality::Balanced => "balanced",
            Quality::Compact => "compact",
        }
    }

    /// The quality `name` names.
    pub fn parse(name: &str) -> Option<Quality> {
        [
            Quality::Lossless,
            Quality::High,
            Quality::Balanced,
            Quality::Compact,
        ]
        .into_iter()
        .find(|q| q.as_str() == name)
    }
}

/// What a target samples, as the converter chooses for it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Sampler {
    /// The CPU raster, drawing into a 16-bit panel when `rgb565`.
    Cpu { rgb565: bool },
    /// A GPU sampling BC.
    Bc,
    /// A GPU sampling ETC2.
    Etc2,
    /// A GPU sampling ASTC.
    Astc,
}

/// How a build converts its images for one target.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Plan {
    pub sampler: Sampler,
    pub quality: Quality,
    /// A format every image takes instead of the one [`Plan::format`] picks.
    pub format: Option<TextureFormat>,
    /// The surface the images show on, in physical pixels: an image larger
    /// than a cover fit into it shows is scaled down. `None` keeps every size.
    pub surface: Option<[u32; 2]>,
    pub effort: Effort,
}

/// How much of an image is translucent.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Alpha {
    /// Every texel opaque.
    Opaque,
    /// Every texel opaque or fully transparent.
    Binary,
    /// Some texel partly transparent.
    Soft,
}

impl Alpha {
    /// The alpha of premultiplied `texels`.
    pub fn of(texels: &[[u8; 4]]) -> Alpha {
        let mut alpha = Alpha::Opaque;
        for t in texels {
            match t[3] {
                255 => {}
                0 => alpha = alpha.max(Alpha::Binary),
                _ => return Alpha::Soft,
            }
        }
        alpha
    }
}

impl Plan {
    /// The format an image of `alpha` takes.
    pub fn format(&self, alpha: Alpha) -> TextureFormat {
        if let Some(format) = self.format {
            return format;
        }
        use TextureFormat as F;
        if self.quality == Quality::Lossless {
            return F::Rgba8Unorm;
        }
        match self.sampler {
            Sampler::Cpu { rgb565 } => {
                let sixteen = rgb565 || self.quality != Quality::High;
                match alpha {
                    Alpha::Opaque if sixteen => F::Rgb565Unorm,
                    Alpha::Binary if sixteen => F::Rgba5551Unorm,
                    Alpha::Soft if self.quality == Quality::Compact => F::Rgba4444Unorm,
                    _ => F::Rgba8Unorm,
                }
            }
            Sampler::Bc => match (self.quality, alpha) {
                (Quality::High, _) | (_, Alpha::Soft) => F::Bc7RgbaUnorm,
                _ => F::Bc1RgbaUnorm,
            },
            Sampler::Etc2 => match alpha {
                Alpha::Opaque => F::Etc2Rgb8Unorm,
                Alpha::Binary => F::Etc2Rgb8A1Unorm,
                Alpha::Soft => F::Etc2Rgba8Unorm,
            },
            Sampler::Astc => match self.quality {
                Quality::Compact => F::Astc8x8Unorm,
                Quality::Balanced => F::Astc6x6Unorm,
                _ => F::Astc4x4Unorm,
            },
        }
    }

    /// `texels`, a `width × height` premultiplied RGBA8 image laid out at
    /// `natural` pixels, converted: scaled to the surface, stretched to whole
    /// blocks, encoded.
    ///
    /// # Panics
    ///
    /// When `texels` does not hold `width × height` texels, or the forced
    /// format is one no image is converted to.
    pub fn convert(
        &self,
        texels: &[[u8; 4]],
        width: u32,
        height: u32,
        natural: [f32; 2],
    ) -> RasterImage {
        assert!(codec::supports(
            self.format.unwrap_or(TextureFormat::Rgba8Unorm)
        ));
        let [w, h] = match self.surface {
            Some(surface) => resample::fit_to_surface(width, height, surface),
            None => [width, height],
        };
        let fitted;
        let texels = if [w, h] == [width, height] {
            texels
        } else {
            fitted = resample::resize(texels, width, height, w, h);
            &fitted
        };
        // The alpha of what is encoded: shrinking softens a hard edge.
        let format = self.format(Alpha::of(texels));
        let block = format.block();
        let (tw, th) = (
            w.next_multiple_of(block.width),
            h.next_multiple_of(block.height),
        );
        let blocks;
        let texels = if (tw, th) == (w, h) {
            texels
        } else {
            blocks = resample::resize(texels, w, h, tw, th);
            &blocks
        };
        RasterImage {
            width: tw,
            height: th,
            natural,
            format,
            texels: Texels::owned(codec::encode(format, tw, th, texels, self.effort)),
        }
    }

    /// The asset `bytes` hold — a PNG or JPEG, or an SVG drawn at `svg`
    /// times its document size when given — converted.
    ///
    /// # Errors
    ///
    /// Bytes in no asset format, a decoder error, or an SVG when `svg` is
    /// `None`.
    pub fn convert_asset(&self, bytes: &[u8], svg: Option<f32>) -> Result<RasterImage, AssetError> {
        use viso_schema::resource::AssetFormat;
        if AssetFormat::sniff(bytes) == Some(AssetFormat::Svg) {
            let scale = svg.ok_or(AssetError::Kind(AssetFormat::Svg))?;
            let tree =
                viso_svg::parse(bytes).map_err(|error| AssetError::Decode(error.to_string()))?;
            let natural = [tree.size.0, tree.size.1];
            let [w, h] = natural.map(|v| ((v * scale).round() as u32).max(1));
            let texels = crate::vector::rasterize(&tree, w, h);
            return Ok(self.convert(&texels, w, h, natural));
        }
        let image = crate::raster::decode_raster(bytes)?;
        let texels = image
            .rgba8_texels()
            .ok_or_else(|| AssetError::Decode("the image holds no texels".to_owned()))?;
        Ok(self.convert(&texels, image.width, image.height, image.natural))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn plan(sampler: Sampler, quality: Quality) -> Plan {
        Plan {
            sampler,
            quality,
            format: None,
            surface: None,
            effort: Effort::Fast,
        }
    }

    #[test]
    fn the_format_follows_the_target_the_quality_and_the_alpha() {
        use TextureFormat as F;
        let cpu = |rgb565, q| plan(Sampler::Cpu { rgb565 }, q);
        assert_eq!(
            cpu(false, Quality::High).format(Alpha::Opaque),
            F::Rgba8Unorm
        );
        assert_eq!(
            cpu(true, Quality::High).format(Alpha::Opaque),
            F::Rgb565Unorm
        );
        assert_eq!(
            cpu(true, Quality::High).format(Alpha::Binary),
            F::Rgba5551Unorm
        );
        assert_eq!(cpu(true, Quality::High).format(Alpha::Soft), F::Rgba8Unorm);
        assert_eq!(
            cpu(false, Quality::Compact).format(Alpha::Soft),
            F::Rgba4444Unorm
        );
        assert_eq!(
            cpu(true, Quality::Lossless).format(Alpha::Opaque),
            F::Rgba8Unorm
        );
        assert_eq!(
            plan(Sampler::Bc, Quality::High).format(Alpha::Opaque),
            F::Bc7RgbaUnorm
        );
        assert_eq!(
            plan(Sampler::Bc, Quality::Balanced).format(Alpha::Binary),
            F::Bc1RgbaUnorm
        );
        assert_eq!(
            plan(Sampler::Bc, Quality::Compact).format(Alpha::Soft),
            F::Bc7RgbaUnorm
        );
        assert_eq!(
            plan(Sampler::Etc2, Quality::High).format(Alpha::Binary),
            F::Etc2Rgb8A1Unorm
        );
        assert_eq!(
            plan(Sampler::Astc, Quality::Balanced).format(Alpha::Soft),
            F::Astc6x6Unorm
        );
        let forced = Plan {
            format: Some(F::Bc3RgbaUnorm),
            ..plan(Sampler::Bc, Quality::High)
        };
        assert_eq!(forced.format(Alpha::Opaque), F::Bc3RgbaUnorm);
    }

    #[test]
    fn alpha_is_classified_from_the_texels() {
        assert_eq!(Alpha::of(&[[1, 2, 3, 255]]), Alpha::Opaque);
        assert_eq!(Alpha::of(&[[1, 2, 3, 255], [0; 4]]), Alpha::Binary);
        assert_eq!(Alpha::of(&[[1, 2, 3, 255], [0, 0, 0, 7]]), Alpha::Soft);
    }

    /// What converting the image at `VISO_IMAGE_SAMPLE` costs and saves, per
    /// format: its bytes as converted (what the binary holds and the device
    /// keeps), the RGBA8 texture it replaces, the PSNR against the decoded
    /// source and the encode time. Release only:
    /// `VISO_IMAGE_SAMPLE=… cargo test --release -p viso-image --lib
    /// measure_sample -- --ignored --nocapture`.
    #[test]
    #[ignore = "a measurement"]
    fn measure_sample() {
        use std::time::Instant;
        let path = std::env::var("VISO_IMAGE_SAMPLE").expect("VISO_IMAGE_SAMPLE");
        let bytes = std::fs::read(&path).expect("reads");
        let start = Instant::now();
        let image = crate::decode_raster(&bytes).expect("decodes");
        let decode = start.elapsed();
        let full = image.rgba8_texels().unwrap();
        // Cropped to whole blocks of every format, so each PSNR compares the
        // same texels.
        let (w, h) = (image.width / 24 * 24, image.height / 24 * 24);
        let source: Vec<[u8; 4]> = (0..h)
            .flat_map(|y| {
                let row = (y * image.width) as usize;
                full[row..row + w as usize].iter().copied()
            })
            .collect();
        println!(
            "{path}: {}×{} ({w}×{h} measured), {} bytes as written ({:.2} bits per texel), decoded in {:.1} ms to {} bytes of RGBA8 ({:?} alpha)",
            image.width,
            image.height,
            bytes.len(),
            bytes.len() as f64 * 8.0 / f64::from(image.width * image.height),
            decode.as_secs_f64() * 1e3,
            image.width * image.height * 4,
            Alpha::of(&source)
        );
        use TextureFormat as F;
        for format in [
            F::Rgba8Unorm,
            F::Rgb565Unorm,
            F::Rgba5551Unorm,
            F::Rgba4444Unorm,
            F::Bc1RgbaUnorm,
            F::Bc3RgbaUnorm,
            F::Bc7RgbaUnorm,
            F::Etc2Rgb8Unorm,
            F::Etc2Rgba8Unorm,
            F::Astc4x4Unorm,
            F::Astc6x6Unorm,
            F::Astc8x8Unorm,
        ] {
            let plan = Plan {
                format: Some(format),
                effort: Effort::Thorough,
                ..plan(Sampler::Bc, Quality::High)
            };
            let start = Instant::now();
            let converted = plan.convert(&source, w, h, [w as f32, h as f32]);
            let encode = start.elapsed();
            let file = crate::texture::write(&converted);
            let start = Instant::now();
            let back = converted.rgba8_texels().unwrap();
            let decode = start.elapsed();
            let psnr = format!("{:.2} dB", codec::psnr(&source, &back));
            println!(
                "{format:?}: {} bytes, {:.2} bits per texel, {psnr}, encode {:.0} ms, software decode {:.1} ms",
                file.len(),
                file.len() as f64 * 8.0 / f64::from(w * h),
                encode.as_secs_f64() * 1e3,
                decode.as_secs_f64() * 1e3,
            );
        }
    }

    #[test]
    fn a_large_image_shrinks_to_the_surface_and_keeps_its_layout_size() {
        let texels = vec![[10, 20, 30, 255]; 64 * 32];
        let plan = Plan {
            surface: Some([16, 16]),
            ..plan(Sampler::Cpu { rgb565: true }, Quality::High)
        };
        let image = plan.convert(&texels, 64, 32, [64.0, 32.0]);
        assert_eq!((image.width, image.height), (32, 16));
        assert_eq!(image.natural, [64.0, 32.0]);
        assert_eq!(image.format, TextureFormat::Rgb565Unorm);
        assert_eq!(image.texels.len(), 32 * 16 * 2);
    }
}
