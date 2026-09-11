//! Real-Metal verification of the A8 glyph-coverage path.
//!
//! The headless rasterizer never compiles MSL, so a change to the GlyphRun
//! fragment body only takes effect on a device pipeline. This test compiles
//! [`GLYPHRUN_MSL`] on the system's default Metal device, draws one glyph
//! instance sampling a hand-built A8 coverage atlas into an offscreen BGRA8
//! texture, reads it back, and asserts the fragment sampled coverage *directly*
//! (`cov = texel.r`) and output premultiplied `color.rgb * (color.a * cov)`.
//!
//! The atlas holds raw coverage bytes, not a signed-distance field. A shader
//! still doing the old SDF decode (`clamp((sd - 0.75) * px_range + 0.5, 0, 1)`)
//! would map these bytes to wildly different alphas — e.g. coverage `1.0`
//! (sd = 1.0) would decode near full only for one specific `px_range`, and
//! coverage `0.5` (sd = 0.5) would decode to a large negative clamped to 0 — so
//! the per-texel assertions below fail unless the direct-sample path shipped.
//!
//! macOS-only: it needs a real `MTLDevice`. On CI without a GPU this is skipped.

#![cfg(target_os = "macos")]

use viso_gpu::backend::{
    DrawCommand, DrawList, Geometry, InlineUniforms, LoadOp, RenderPass, RenderTarget,
};
use viso_gpu::{
    AddressMode, BindGroupDesc, Binding, BlendMode, BufferDesc, BufferUsage, BuiltinShader,
    FilterMode, GpuBackend, MetalBackend, PipelineDesc, SamplerDesc, TextureDesc, TextureFormat,
};
use viso_render::{GlyphInstance, glyphrun_schema};
use viso_shader::GLYPHRUN_MSL;

/// Reinterpret a `#[repr(C)]` `GlyphInstance` as its raw instance bytes.
fn instance_bytes(inst: &GlyphInstance) -> &[u8] {
    // SAFETY: `GlyphInstance` is `#[repr(C)]` of `[f32; N]` fields with no
    // padding (validated against `glyphrun_schema` at pipeline creation), so its
    // byte representation is a valid contiguous instance buffer.
    unsafe {
        std::slice::from_raw_parts(
            (inst as *const GlyphInstance) as *const u8,
            std::mem::size_of::<GlyphInstance>(),
        )
    }
}

#[test]
fn glyph_a8_coverage_direct_sample_on_metal() {
    let mut gpu = MetalBackend::new();

    // Offscreen 4x1 BGRA8 target, one output texel per atlas coverage sample.
    const W: u32 = 4;
    const H: u32 = 1;
    let target = gpu.create_texture(&TextureDesc {
        width: W,
        height: H,
        format: TextureFormat::Bgra8Unorm,
        render_target: true,
        label: "metal-glyph-target",
    });

    // A 4x1 A8 coverage atlas with exact, known coverage per texel. These are
    // raw coverage bytes (the new A8 contract), NOT a distance field.
    let cov = [0u8, 128u8, 191u8, 255u8];
    let atlas = gpu.create_texture(&TextureDesc {
        width: 4,
        height: 1,
        format: TextureFormat::R8Unorm,
        render_target: false,
        label: "metal-glyph-atlas",
    });
    gpu.write_texture(atlas, 0, 0, 4, 1, &cov);

    // Linear-clamp sampler (matches the renderer's glyph sampler).
    let sampler = gpu.create_sampler(&SamplerDesc {
        filter: FilterMode::Linear,
        address: AddressMode::ClampToEdge,
    });

    let bind_group = gpu.create_bind_group(&BindGroupDesc {
        label: "metal-glyph-bg",
        bindings: vec![Binding::Texture(atlas), Binding::Sampler(sampler)],
    });

    // The real device pipeline: the same MSL, entries, blend, and schema the
    // renderer registers. Compiling this is the point of the test.
    let pipeline = gpu
        .create_pipeline(
            &PipelineDesc {
                label: "metal-glyph",
                builtin: BuiltinShader::GlyphRun,
                shader_source: GLYPHRUN_MSL(),
                vertex_entry: "vertex_main",
                fragment_entry: "fragment_main",
                color_format: TextureFormat::Bgra8Unorm,
                depth_format: None,
                blend: BlendMode::PremultipliedOver,
                instance_schema: glyphrun_schema(),
            },
            &GlyphInstance::LAYOUT,
        )
        .expect("GlyphInstance layout matches the glyph shader schema");

    // One instance covering the whole 4x1 target. The UV rect spans the atlas so
    // each output texel center samples its matching atlas texel center: with the
    // texel-center convention, output x-center i+0.5 maps to atlas u =
    // (i+0.5)/4, i.e. atlas texel i's center — a 1:1 exact sample, no blur.
    let color = [0.2f32, 0.4, 0.9, 1.0];
    let inst = GlyphInstance {
        rect_pos: [0.0, 0.0],
        rect_size: [W as f32, H as f32],
        uv_pos: [0.0, 0.0],
        uv_size: [1.0, 1.0],
        color,
    };
    let ibytes = instance_bytes(&inst);
    let instance_buffer = gpu.create_buffer(&BufferDesc {
        size: ibytes.len(),
        usage: BufferUsage::INSTANCE | BufferUsage::CPU_WRITE,
        label: "metal-glyph-inst",
    });
    gpu.write_buffer(instance_buffer, 0, ibytes);

    // Viewport uniform: [width, height] in pixels (float2), pixel→NDC.
    let vp = [W as f32, H as f32];
    let vp_bytes: &[u8] = bytemuck_cast(&vp);

    let commands = [DrawCommand {
        pipeline,
        bind_group: Some(bind_group),
        geometry: Geometry::Generated { count: 1 },
        instance_buffer,
        instance_offset: 0,
        uniforms: InlineUniforms::new(vp_bytes),
        scissor: None,
    }];
    let passes = [RenderPass {
        target: RenderTarget::Texture(target),
        // Clear to transparent black so the premultiplied output over it is the
        // glyph's own premultiplied color.
        load: LoadOp::Clear([0.0, 0.0, 0.0, 0.0]),
        first_command: 0,
        command_count: 1,
    }];
    gpu.encode(&DrawList {
        commands: &commands,
        passes: &passes,
    });

    let pixels = gpu.read_texture(target); // BGRA8, top-left origin, 4 texels.
    assert_eq!(pixels.len(), (W * H * 4) as usize, "readback size");

    // Expected per output texel: premultiplied over transparent black. With
    // straight color `c` and sampled coverage `k = cov/255`, alpha `a = c.a * k`
    // and stored RGB (premultiplied) = c.rgb * a. Byte order is BGRA.
    for (i, &c8) in cov.iter().enumerate() {
        let k = c8 as f32 / 255.0;
        let a = color[3] * k;
        let want = [
            (color[2] * a * 255.0).round() as i32, // B
            (color[1] * a * 255.0).round() as i32, // G
            (color[0] * a * 255.0).round() as i32, // R
            (a * 255.0).round() as i32,            // A
        ];
        let got = [
            pixels[i * 4] as i32,
            pixels[i * 4 + 1] as i32,
            pixels[i * 4 + 2] as i32,
            pixels[i * 4 + 3] as i32,
        ];
        // A small tolerance for the linear sampler's fixed-point rounding.
        for ch in 0..4 {
            let diff = (want[ch] - got[ch]).abs();
            assert!(
                diff <= 2,
                "texel {i} channel {ch}: coverage {c8} → want {want:?}, got {got:?} \
                 (diff {diff} > 2). The device shader is not sampling A8 coverage \
                 directly (cov = texel.r).",
                want = want,
                got = got,
            );
        }
    }
}

/// Reinterpret a `[f32; 2]` as its little-endian bytes (no external dep).
fn bytemuck_cast(v: &[f32; 2]) -> &[u8] {
    // SAFETY: `[f32; 2]` is `#[repr(C)]`-equivalent POD, 8 contiguous bytes, and
    // the returned slice borrows it for the same lifetime.
    unsafe { std::slice::from_raw_parts((v as *const [f32; 2]) as *const u8, 8) }
}
