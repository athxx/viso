//! Converted images drawn as the device samples them: every texel format the
//! build converts to, encoded by `viso_image::codec`, uploaded, drawn 1:1 and
//! read back, against the software decoder — the fallback a device lacking
//! the format takes, so the device and the fallback must agree.
//!
//! On Metal (Apple GPUs sample BC, ETC2 and ASTC) the hardware decoder is an
//! independent check of every encoder's bitstream. The CPU raster samples the
//! 16-bit formats in place, from texels embedded in the binary.

use viso_gpu::{
    AddressMode, CpuRaster, FilterMode, GpuBackend, SamplerDesc, TextureDesc, TextureFormat,
    TextureId,
};
use viso_image::codec::{self, Effort};
use viso_render::{ImageDraw, Primitive, Rect, Renderer};

/// A multiple of every block edge (4, 6, 8).
const SIZE: u32 = 48;

/// A photo-like image with soft and hard alpha edges, premultiplied.
fn sample() -> Vec<[u8; 4]> {
    let mut texels = Vec::with_capacity((SIZE * SIZE) as usize);
    for y in 0..SIZE {
        for x in 0..SIZE {
            let (fx, fy) = (x as f32 / SIZE as f32, y as f32 / SIZE as f32);
            let a = if x < 6 {
                0
            } else if y > SIZE - 10 {
                (fx * 255.0) as u8
            } else {
                255
            };
            let c = |v: f32| ((v.clamp(0.0, 1.0) * f32::from(a)).round()) as u8;
            let ring = ((fx - 0.5).hypot(fy - 0.5) * 18.0).sin() * 0.5 + 0.5;
            texels.push([c(fx), c(ring), c(1.0 - fy), a]);
        }
    }
    texels
}

/// The formats the build converts to that `gpu` samples.
fn sampled(gpu: &impl GpuBackend) -> Vec<TextureFormat> {
    use TextureFormat as F;
    [
        F::Rgba8Unorm,
        F::Rgb565Unorm,
        F::Rgba4444Unorm,
        F::Rgba5551Unorm,
        F::Bc1RgbaUnorm,
        F::Bc3RgbaUnorm,
        F::Bc7RgbaUnorm,
        F::Etc2Rgb8Unorm,
        F::Etc2Rgb8A1Unorm,
        F::Etc2Rgba8Unorm,
        F::Astc4x4Unorm,
        F::Astc6x6Unorm,
        F::Astc8x8Unorm,
    ]
    .into_iter()
    .filter(|f| gpu.caps().samples(*f))
    .collect()
}

/// `texture` drawn 1:1, nearest, over transparent black into a BGRA8
/// target; its pixels, BGRA8.
fn draw<B: GpuBackend>(gpu: &mut B, texture: TextureId) -> TextureId {
    let mut renderer = Renderer::new(gpu, TextureFormat::Bgra8Unorm);
    let size = SIZE as f32;
    let mut image = ImageDraw::new(
        Rect {
            x: 0.0,
            y: 0.0,
            w: size,
            h: size,
        },
        texture,
    );
    image.sampler = SamplerDesc {
        filter: FilterMode::Nearest,
        address: AddressMode::ClampToEdge,
    };
    renderer.upload(gpu, &[Primitive::Image(image)]);
    let target = gpu.create_texture(&TextureDesc {
        width: SIZE,
        height: SIZE,
        format: TextureFormat::Bgra8Unorm,
        render_target: true,
        label: "texture-formats-target",
    });
    renderer.render_to_texture(gpu, target, [0.0; 4], [size, size]);
    target
}

/// The largest channel difference between BGRA8 `drawn` and RGBA8 `decoded`.
fn worst(drawn: &[u8], decoded: &[[u8; 4]]) -> (u8, usize) {
    let mut worst = (0, 0);
    for (i, (d, e)) in drawn.as_chunks::<4>().0.iter().zip(decoded).enumerate() {
        let rgba = [d[2], d[1], d[0], d[3]];
        for c in 0..4 {
            let diff = rgba[c].abs_diff(e[c]);
            if diff > worst.0 {
                worst = (diff, i);
            }
        }
    }
    worst
}

/// Draws every format `gpu` samples, uploading through `upload`, and checks
/// each against the software decoder within `tolerance`.
fn every_format_draws_as_it_decodes<B: GpuBackend>(
    gpu: &mut B,
    read: impl Fn(&B, TextureId) -> Vec<u8>,
    tolerance: impl Fn(TextureFormat) -> u8,
) -> usize {
    let texels = sample();
    let formats = sampled(gpu);
    for &format in &formats {
        let data = codec::encode(format, SIZE, SIZE, &texels, Effort::Fast);
        assert_eq!(data.len(), format.data_size(SIZE, SIZE), "{format:?}");
        let decoded = codec::decode(format, SIZE, SIZE, &data).expect("decodes");
        let desc = TextureDesc {
            width: SIZE,
            height: SIZE,
            format,
            render_target: false,
            label: "converted",
        };
        // As an embedded converted image is: borrowed for the program's life.
        let texture = gpu.create_texture_static(&desc, Box::leak(data.into_boxed_slice()));
        let target = draw(gpu, texture);
        let (diff, at) = worst(&read(gpu, target), &decoded);
        println!("{format:?}: the device and the decoder differ by at most {diff}");
        assert!(
            diff <= tolerance(format),
            "{format:?}: the device and the decoder differ by {diff} at texel {at}"
        );
    }
    formats.len()
}

#[test]
fn the_cpu_raster_samples_16_bit_texels_in_place_as_they_decode() {
    let mut gpu = CpuRaster::new();
    let drawn = every_format_draws_as_it_decodes(&mut gpu, |g, t| g.read_texture(t), |_| 0);
    assert_eq!(drawn, 4, "RGBA8 and the three 16-bit formats");
    assert!(!gpu.caps().samples(TextureFormat::Bc7RgbaUnorm));
    assert!(!gpu.caps().samples(TextureFormat::Astc4x4Unorm));
}

#[cfg(target_vendor = "apple")]
#[test]
fn metal_samples_every_converted_format_as_the_software_decoder_does() {
    let mut gpu = viso_gpu::MetalBackend::new();
    // Interpolated endpoints (BC1/BC3, 16-bit expansion, ASTC weights) round
    // in the hardware's precision, which the formats leave within a level.
    let tolerance = |_| 1;
    let drawn = every_format_draws_as_it_decodes(&mut gpu, |g, t| g.read_texture(t), tolerance);
    // Apple silicon samples all three compressed families; an Intel Mac BC
    // and the 16-bit formats.
    assert!(drawn >= 7, "{drawn}");
}
