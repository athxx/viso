//! Smoke test for the `HeadlessRaster` backend: register a Quad pipeline, draw
//! one filled rectangle over a cleared background, and read pixels back.
//!
//! This exercises the whole headless closed loop (create_surface → create_buffer
//! → write_buffer → create_pipeline (with layout validation) → encode →
//! read_pixels) that golden tests will build on, without needing a GPU.

use viso_gpu::backend::{
    DrawCommand, DrawList, Geometry, InlineUniforms, RenderPass, RenderTarget,
};
use viso_gpu::{
    AttrFormat, BlendMode, BufferDesc, BufferUsage, BuiltinShader, GpuBackend, GpuInstance,
    HeadlessRaster, InstanceSchema, LoadOp, PipelineDesc, RawWindowHandle, SchemaAttr,
    TextureFormat,
};

/// The Quad built-in's instance layout, matching the field names/formats the
/// headless `fill_quad` reads and the Quad shader schema declares.
#[repr(C)]
#[derive(Clone, Copy, GpuInstance)]
struct QuadInstance {
    rect_pos: [f32; 2],
    rect_size: [f32; 2],
    color: [f32; 4],
    radius: f32,
    border_width: f32,
    border_color: [f32; 4],
}

/// The schema a Quad shader declares (must match `QuadInstance`'s layout).
fn quad_schema() -> InstanceSchema {
    InstanceSchema {
        attributes: &[
            SchemaAttr {
                name: "rect_pos",
                format: AttrFormat::Float2,
            },
            SchemaAttr {
                name: "rect_size",
                format: AttrFormat::Float2,
            },
            SchemaAttr {
                name: "color",
                format: AttrFormat::Float4,
            },
            SchemaAttr {
                name: "radius",
                format: AttrFormat::Float1,
            },
            SchemaAttr {
                name: "border_width",
                format: AttrFormat::Float1,
            },
            SchemaAttr {
                name: "border_color",
                format: AttrFormat::Float4,
            },
        ],
    }
}

fn as_bytes(inst: &QuadInstance) -> &[u8] {
    // Safe: `QuadInstance` is `#[repr(C)]` and `Copy` (GpuInstance invariant).
    unsafe {
        core::slice::from_raw_parts(
            (inst as *const QuadInstance) as *const u8,
            core::mem::size_of::<QuadInstance>(),
        )
    }
}

#[test]
fn draws_a_solid_rect_over_a_cleared_background() {
    let mut gpu = HeadlessRaster::new();
    let (w, h) = (64u32, 48u32);
    let surface = gpu.create_surface(RawWindowHandle::Headless, w, h);

    // A sharp-cornered opaque green rect from (16,12) to (48,36).
    let inst = QuadInstance {
        rect_pos: [16.0, 12.0],
        rect_size: [32.0, 24.0],
        color: [0.0, 1.0, 0.0, 1.0],
        radius: 0.0,
        border_width: 0.0,
        border_color: [0.0, 0.0, 0.0, 0.0],
    };

    let inst_buf = gpu.create_buffer(&BufferDesc {
        size: core::mem::size_of::<QuadInstance>(),
        usage: BufferUsage::INSTANCE | BufferUsage::CPU_WRITE,
        label: "quad-instances",
    });
    gpu.write_buffer(inst_buf, 0, as_bytes(&inst));

    let pipeline = gpu
        .create_pipeline(
            &PipelineDesc {
                label: "quad",
                builtin: BuiltinShader::Quad,
                shader_source: "",
                vertex_entry: "vertex_main",
                fragment_entry: "fragment_main",
                color_format: TextureFormat::Bgra8Unorm,
                depth_format: None,
                blend: BlendMode::PremultipliedOver,
                instance_schema: quad_schema(),
            },
            &QuadInstance::LAYOUT,
        )
        .expect("layout matches schema");

    let frame = gpu.begin_frame(surface);
    let cmd = DrawCommand {
        pipeline,
        bind_group: None,
        geometry: Geometry::Generated { count: 1 },
        instance_buffer: inst_buf,
        instance_offset: 0,
        uniforms: InlineUniforms::EMPTY,
        scissor: None,
    };
    gpu.encode(&DrawList {
        commands: &[cmd],
        passes: &[RenderPass {
            target: RenderTarget::Surface(frame),
            // Clear to opaque blue.
            load: LoadOp::Clear([0.0, 0.0, 1.0, 1.0]),
            first_command: 0,
            command_count: 1,
        }],
    });
    gpu.present(frame);

    // Center of the rect is green.
    let center = gpu.surface_texel(surface, 32, 24);
    assert!(
        center[1] > 0.99 && center[0] < 0.01 && center[2] < 0.01,
        "{center:?}"
    );

    // A corner well outside the rect is still the blue clear color.
    let outside = gpu.surface_texel(surface, 2, 2);
    assert!(
        outside[2] > 0.99 && outside[0] < 0.01 && outside[1] < 0.01,
        "{outside:?}"
    );

    // Readback is BGRA8, top-left, un-premultiplied.
    let px = gpu.read_pixels_bgra8(surface);
    assert_eq!(px.len(), (w * h * 4) as usize);
    // Center pixel (32,24): B=0 G=255 R=0 A=255.
    let i = ((24 * w + 32) * 4) as usize;
    assert_eq!(&px[i..i + 4], &[0, 255, 0, 255]);
}
