//! Real-WebGPU verification of the WebGPU backend.
//!
//! Every test opens a device, runs the frozen WGSL through the shared pipeline
//! layout, reads the pixels back, and asserts the device reported no validation
//! error (`uncapturederror`), so each one also checks the bind-group layout, the
//! dynamic uniform offsets and the vertex-buffer layouts.
//!
//! Built for `wasm32-unknown-unknown` only; run with `wasm-bindgen-test-runner`
//! under a WebGPU-capable host (Deno, or a browser). Skipped without WebGPU.

#![cfg(all(target_arch = "wasm32", target_os = "unknown"))]

mod golden_scenes;

use viso_gpu::backend::{
    DrawCommand, DrawList, Geometry, InlineUniforms, LoadOp, RenderPass, RenderTarget,
};
use viso_gpu::{
    AddressMode, BindGroupDesc, Binding, BlendMode, BufferDesc, BufferId, BufferUsage, FilterMode,
    GpuBackend, InstanceLayout, PipelineDesc, PipelineId, SamplerDesc, ShaderLang, TextureDesc,
    TextureFormat, TextureId, WebGpuBackend,
};
use viso_render::{Border, GlyphInstance, ImageInstance, Primitive, Quad, Rect, Renderer, Rgba};
use viso_shader::{PipelineFamily, standard_manifest};
use wasm_bindgen_test::wasm_bindgen_test;

/// A WebGPU device, or `None` (the test is skipped) without one.
async fn device() -> Option<WebGpuBackend> {
    let gpu = WebGpuBackend::request().await;
    if gpu.is_none() {
        eprintln!("no WebGPU device; skipped");
    }
    gpu
}

/// Validation errors arrive as events: a read-back round trip yields to the
/// event loop after the work under test, so they are all counted by now.
async fn assert_no_validation_errors(gpu: &mut WebGpuBackend) {
    let probe = target(gpu, 1, 1);
    gpu.read_texture(probe).await;
    gpu.destroy_texture(probe);
    assert_eq!(
        gpu.validation_errors(),
        0,
        "the device reported validation errors"
    );
}
/// The raw bytes of a `#[repr(C)]` instance.
fn bytes_of<T: Copy>(v: &T) -> &[u8] {
    // SAFETY: the instance types are `#[repr(C)]` structs of `f32` arrays with no
    // padding (validated against their schemas at pipeline creation), so every
    // byte is initialized; the slice borrows `v` for its lifetime.
    unsafe { std::slice::from_raw_parts((v as *const T).cast::<u8>(), size_of::<T>()) }
}

fn viewport(w: u32, h: u32) -> [u8; 8] {
    let mut out = [0u8; 8];
    out[..4].copy_from_slice(&(w as f32).to_le_bytes());
    out[4..].copy_from_slice(&(h as f32).to_le_bytes());
    out
}

fn pipeline(
    gpu: &mut WebGpuBackend,
    family: PipelineFamily,
    format: TextureFormat,
    layout: &InstanceLayout,
) -> PipelineId {
    let entry = standard_manifest()
        .entry(family)
        .expect("the standard manifest populates the family");
    gpu.create_pipeline(
        &PipelineDesc {
            label: "webgpu-test",
            builtin: entry.builtin,
            variant: entry.variant.packed(),
            code: entry.code(ShaderLang::Wgsl),
            vertex_entry: entry.vertex_entry,
            fragment_entry: entry.fragment_entry,
            color_format: format,
            depth_format: None,
            blend: BlendMode::PremultipliedOver,
            instance_schema: entry.schema,
        },
        layout,
    )
    .expect("the instance layout matches the shader schema")
}

fn instance_buffer<T: Copy>(gpu: &mut WebGpuBackend, inst: &T) -> BufferId {
    let bytes = bytes_of(inst);
    let buf = gpu.create_buffer(&BufferDesc {
        size: bytes.len(),
        usage: BufferUsage::INSTANCE | BufferUsage::CPU_WRITE,
        label: "webgpu-test-inst",
    });
    gpu.write_buffer(buf, 0, bytes);
    buf
}

fn target(gpu: &mut WebGpuBackend, w: u32, h: u32) -> TextureId {
    gpu.create_texture(&TextureDesc {
        width: w,
        height: h,
        format: TextureFormat::Bgra8Unorm,
        render_target: true,
        label: "webgpu-test-target",
    })
}

/// Draw one generated instance into `target`, clearing it first.
#[allow(clippy::too_many_arguments)]
fn draw_one(
    gpu: &mut WebGpuBackend,
    target: TextureId,
    w: u32,
    h: u32,
    pipeline: PipelineId,
    bind_group: viso_gpu::BindGroupId,
    instances: BufferId,
    scissor: Option<(u32, u32, u32, u32)>,
) {
    let vp = viewport(w, h);
    let commands = [DrawCommand {
        pipeline,
        bind_group: Some(bind_group),
        geometry: Geometry::Generated { count: 1 },
        instance_buffer: instances,
        instance_offset: 0,
        uniforms: InlineUniforms::new(&vp),
        scissor,
    }];
    let passes = [RenderPass {
        target: RenderTarget::Texture(target),
        load: LoadOp::Clear([0.0, 0.0, 0.0, 0.0]),
        first_command: 0,
        command_count: 1,
    }];
    gpu.encode(&DrawList {
        commands: &commands,
        passes: &passes,
    });
}

/// The frozen glyph WGSL samples A8 coverage directly and outputs
/// premultiplied `color.rgb * (color.a * cov)` — the same oracle the Metal test
/// checks, so the two device paths agree texel for texel.
#[wasm_bindgen_test]
async fn glyph_a8_coverage_direct_sample_on_webgpu() {
    let Some(mut gpu) = device().await else {
        return;
    };
    const W: u32 = 4;
    let out = target(&mut gpu, W, 1);
    let cov = [0u8, 128, 191, 255];
    let atlas = gpu.create_texture(&TextureDesc {
        width: 4,
        height: 1,
        format: TextureFormat::R8Unorm,
        render_target: false,
        label: "webgpu-glyph-atlas",
    });
    gpu.write_texture(atlas, 0, 0, 4, 1, &cov);
    let sampler = gpu.create_sampler(&SamplerDesc {
        filter: FilterMode::Linear,
        address: AddressMode::ClampToEdge,
    });
    let bg = gpu.create_bind_group(&BindGroupDesc {
        label: "webgpu-glyph-bg",
        bindings: vec![Binding::Texture(atlas), Binding::Sampler(sampler)],
    });
    let pipe = pipeline(
        &mut gpu,
        PipelineFamily::MaskComposite,
        TextureFormat::Bgra8Unorm,
        &GlyphInstance::LAYOUT,
    );
    let color = [0.2f32, 0.4, 0.9, 1.0];
    let inst = instance_buffer(
        &mut gpu,
        &GlyphInstance {
            rect_pos: [0.0, 0.0],
            rect_size: [W as f32, 1.0],
            uv_pos: [0.0, 0.0],
            uv_size: [1.0, 1.0],
            color,
        },
    );
    draw_one(&mut gpu, out, W, 1, pipe, bg, inst, None);

    let px = gpu.read_texture(out).await;
    assert_eq!(px.len(), (W * 4) as usize);
    for (i, &c8) in cov.iter().enumerate() {
        let a = color[3] * c8 as f32 / 255.0;
        let want =
            [color[2] * a, color[1] * a, color[0] * a, a].map(|v| (v * 255.0).round() as i32);
        let got: [i32; 4] = std::array::from_fn(|c| px[i * 4 + c] as i32);
        for c in 0..4 {
            assert!(
                (want[c] - got[c]).abs() <= 2,
                "texel {i}: coverage {c8} → want {want:?}, got {got:?}"
            );
        }
    }
    assert_no_validation_errors(&mut gpu).await;
}

/// A texture rendered into is sampled by a later pass of the next command: the
/// render-target → shader-read transition holds, the image lands top-left
/// oriented, and the scissor clips in pixel space.
#[wasm_bindgen_test]
async fn rendered_texture_is_sampled_upright_and_scissored() {
    let Some(mut gpu) = device().await else {
        return;
    };
    const W: u32 = 4;
    const H: u32 = 2;
    // Source: a 4x2 RGBA image, top row red, bottom row blue.
    let src = gpu.create_texture(&TextureDesc {
        width: W,
        height: H,
        format: TextureFormat::Rgba8Unorm,
        render_target: false,
        label: "webgpu-src",
    });
    let mut texels = Vec::new();
    for y in 0..H {
        for _ in 0..W {
            texels.extend_from_slice(if y == 0 {
                &[255, 0, 0, 255]
            } else {
                &[0, 0, 255, 255]
            });
        }
    }
    gpu.write_texture(src, 0, 0, W, H, &texels);
    let sampler = gpu.create_sampler(&SamplerDesc {
        filter: FilterMode::Nearest,
        address: AddressMode::ClampToEdge,
    });
    let pipe = pipeline(
        &mut gpu,
        PipelineFamily::Image,
        TextureFormat::Bgra8Unorm,
        &ImageInstance::LAYOUT,
    );
    let inst = instance_buffer(
        &mut gpu,
        &ImageInstance {
            rect_pos: [0.0, 0.0],
            rect_size: [W as f32, H as f32],
            uv_pos: [0.0, 0.0],
            uv_size: [1.0, 1.0],
            color: [1.0; 4],
        },
    );

    // Pass 1: copy the source into an intermediate render target.
    let mid = target(&mut gpu, W, H);
    let bg_src = gpu.create_bind_group(&BindGroupDesc {
        label: "webgpu-src-bg",
        bindings: vec![Binding::Texture(src), Binding::Sampler(sampler)],
    });
    draw_one(&mut gpu, mid, W, H, pipe, bg_src, inst, None);

    // Pass 2: sample the intermediate into the output, scissored to the left half.
    let out = target(&mut gpu, W, H);
    let bg_mid = gpu.create_bind_group(&BindGroupDesc {
        label: "webgpu-mid-bg",
        bindings: vec![Binding::Texture(mid), Binding::Sampler(sampler)],
    });
    draw_one(&mut gpu, out, W, H, pipe, bg_mid, inst, Some((0, 0, 2, H)));

    let px = gpu.read_texture(out).await;
    let at = |x: u32, y: u32| {
        let i = ((y * W + x) * 4) as usize;
        [px[i], px[i + 1], px[i + 2], px[i + 3]]
    };
    // BGRA bytes.
    assert_eq!(at(0, 0), [0, 0, 255, 255], "top row is red");
    assert_eq!(at(1, 1), [255, 0, 0, 255], "bottom row is blue");
    assert_eq!(at(3, 0), [0, 0, 0, 0], "outside the scissor stays clear");
    assert_no_validation_errors(&mut gpu).await;
}

/// Destroyed resources are parked until the GPU is done with the commands that
/// use them, then released; nothing is left behind after a few submissions.
#[wasm_bindgen_test]
async fn retired_resources_are_released_after_the_gpu_finishes() {
    let Some(mut gpu) = device().await else {
        return;
    };
    let tex = target(&mut gpu, 8, 8);
    let buf = gpu.create_buffer(&BufferDesc {
        size: 64,
        usage: BufferUsage::INSTANCE | BufferUsage::CPU_WRITE,
        label: "webgpu-retire",
    });
    gpu.destroy_texture(tex);
    gpu.destroy_buffer(buf);
    assert_eq!(gpu.retired_count(), 2);
    for _ in 0..3 {
        let probe = target(&mut gpu, 1, 1);
        gpu.read_texture(probe).await;
        gpu.destroy_texture(probe);
        gpu.encode(&DrawList {
            commands: &[],
            passes: &[],
        });
    }
    let probe = target(&mut gpu, 1, 1);
    gpu.read_texture(probe).await;
    gpu.encode(&DrawList {
        commands: &[],
        passes: &[],
    });
    assert!(
        gpu.retired_count() <= 1,
        "retired resources outlived their last use: {}",
        gpu.retired_count()
    );
    assert_no_validation_errors(&mut gpu).await;
}

/// The renderer's device-init prewarm builds every standard pipeline from the
/// frozen WGSL, and a frame's upload path runs, with no validation error.
#[wasm_bindgen_test]
async fn renderer_prewarms_every_standard_pipeline_on_webgpu() {
    let Some(mut gpu) = device().await else {
        return;
    };
    let mut renderer = Renderer::new(&mut gpu, TextureFormat::Bgra8Unorm);
    let scene = [Primitive::Quad(Quad {
        rect: Rect {
            x: 8.0,
            y: 8.0,
            w: 120.0,
            h: 40.0,
        },
        color: Rgba {
            r: 0.2,
            g: 0.5,
            b: 0.9,
            a: 1.0,
        },
        radius: 6.0,
        border: Border {
            width: 1.0,
            color: Rgba {
                r: 0.0,
                g: 0.0,
                b: 0.0,
                a: 1.0,
            },
        },
    })];
    for _ in 0..3 {
        renderer.upload(&mut gpu, &scene);
    }
    assert_no_validation_errors(&mut gpu).await;
}

/// The validation check above is live: an invalid program is reported through
/// `uncapturederror` and counted.
#[wasm_bindgen_test]
async fn invalid_wgsl_is_counted_as_a_validation_error() {
    let Some(mut gpu) = device().await else {
        return;
    };
    let entry = standard_manifest()
        .entry(PipelineFamily::Image)
        .expect("the standard manifest populates the family");
    gpu.create_pipeline(
        &PipelineDesc {
            label: "webgpu-invalid",
            builtin: entry.builtin,
            variant: entry.variant.packed(),
            code: viso_gpu::ShaderCode::Wgsl("fn vertex_main( {"),
            vertex_entry: entry.vertex_entry,
            fragment_entry: entry.fragment_entry,
            color_format: TextureFormat::Bgra8Unorm,
            depth_format: None,
            blend: BlendMode::PremultipliedOver,
            instance_schema: entry.schema,
        },
        &ImageInstance::LAYOUT,
    )
    .expect("the instance layout matches the shader schema");
    let probe = target(&mut gpu, 1, 1);
    gpu.read_texture(probe).await;
    assert!(
        gpu.validation_errors() > 0,
        "an invalid shader module went unreported"
    );
}

/// Every golden scene, rendered through the renderer on the device, matches the
/// headless reference.
#[wasm_bindgen_test]
async fn golden_scenes_match_on_webgpu() {
    let Some(mut gpu) = device().await else {
        return;
    };
    for scene in golden_scenes::GoldenScene::ALL {
        let target = golden_scenes::render_to_target(&mut gpu, scene);
        let pixels = gpu.read_texture(target).await;
        golden_scenes::assert_device_matches(scene, &pixels);
    }
    assert_no_validation_errors(&mut gpu).await;
}
