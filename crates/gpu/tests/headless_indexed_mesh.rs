//! Smoke test for indexed-mesh draws through `HeadlessRaster`, covering both
//! index widths. The vector-mesh lane (Path/Mesh) tessellates into a real
//! vertex + index buffer and draws it as one indexed triangle list; small
//! geometry (≤ 64k vertices) uses a 16-bit index buffer, larger geometry a
//! 32-bit one (§13.4). This proves the backend binds each width correctly:
//! the same two-triangle quad, drawn once with `U16` indices and once with
//! `U32` indices, rasterizes identically.

use viso_gpu::backend::{
    DrawCommand, DrawList, Geometry, IndexFormat, InlineUniforms, RenderPass, RenderTarget,
};
use viso_gpu::{
    AttrFormat, BlendMode, BufferDesc, BufferUsage, BuiltinShader, GpuBackend, GpuPod,
    HeadlessRaster, InstanceSchema, LoadOp, PipelineDesc, RawWindowHandle, SchemaAttr,
    TextureFormat,
};

/// The mesh vertex layout the headless `fill_mesh` reads by name: straight
/// linear `pos`/`color` and a coverage `edge` weight (1 in the interior).
/// `#[derive(GpuPod)]` emits the validated `LAYOUT` (stride 28: pos@0, color@8,
/// edge@24), matching the frozen `MeshVertex` ABI without depending on
/// `viso-render`.
#[repr(C)]
#[derive(Clone, Copy, GpuPod)]
struct MeshVertex {
    pos: [f32; 2],
    color: [f32; 4],
    edge: f32,
}

fn mesh_schema() -> InstanceSchema {
    InstanceSchema {
        attributes: &[
            SchemaAttr {
                name: "pos",
                format: AttrFormat::Float2,
            },
            SchemaAttr {
                name: "color",
                format: AttrFormat::Float4,
            },
            SchemaAttr {
                name: "edge",
                format: AttrFormat::Float1,
            },
        ],
    }
}

fn vertex_bytes(verts: &[MeshVertex]) -> &[u8] {
    // Safe: `MeshVertex` is `#[repr(C)]` and `Copy`.
    unsafe {
        core::slice::from_raw_parts(verts.as_ptr() as *const u8, core::mem::size_of_val(verts))
    }
}

/// Draw a green quad (0,0)-(w,h)/2 as two triangles with the given index width,
/// over a blue clear, and return the readback. `indices` are the six triangle
/// indices in the chosen width's byte encoding.
fn draw_indexed(format: IndexFormat, index_bytes: &[u8]) -> ([f32; 4], [f32; 4]) {
    let mut gpu = HeadlessRaster::new();
    let (w, h) = (64u32, 48u32);
    let surface = gpu.create_surface(RawWindowHandle::Headless, w, h);

    // A green square from (16,12) to (48,36), two triangles (0,1,2)+(0,2,3).
    let green = [0.0, 1.0, 0.0, 1.0];
    let verts = [
        MeshVertex {
            pos: [16.0, 12.0],
            color: green,
            edge: 1.0,
        },
        MeshVertex {
            pos: [48.0, 12.0],
            color: green,
            edge: 1.0,
        },
        MeshVertex {
            pos: [48.0, 36.0],
            color: green,
            edge: 1.0,
        },
        MeshVertex {
            pos: [16.0, 36.0],
            color: green,
            edge: 1.0,
        },
    ];

    let vtx_buf = gpu.create_buffer(&BufferDesc {
        size: core::mem::size_of_val(&verts),
        usage: BufferUsage::VERTEX | BufferUsage::CPU_WRITE,
        label: "mesh-vertices",
    });
    gpu.write_buffer(vtx_buf, 0, vertex_bytes(&verts));

    let idx_buf = gpu.create_buffer(&BufferDesc {
        size: index_bytes.len(),
        usage: BufferUsage::INDEX | BufferUsage::CPU_WRITE,
        label: "mesh-indices",
    });
    gpu.write_buffer(idx_buf, 0, index_bytes);

    let pipeline = gpu
        .create_pipeline(
            &PipelineDesc {
                label: "mesh",
                builtin: BuiltinShader::Mesh,
                variant: 0,
                code: viso_gpu::ShaderCode::None,
                vertex_entry: "vertex_main",
                fragment_entry: "fragment_main",
                color_format: TextureFormat::Bgra8Unorm,
                depth_format: None,
                blend: BlendMode::PremultipliedOver,
                instance_schema: mesh_schema(),
            },
            &MeshVertex::LAYOUT,
        )
        .expect("mesh layout matches schema");

    let frame = gpu
        .begin_frame(surface)
        .expect("headless acquire never fails");
    let cmd = DrawCommand {
        pipeline,
        bind_group: None,
        geometry: Geometry::IndexedMesh {
            vertex_buffer: vtx_buf,
            index_buffer: idx_buf,
            index_format: format,
            index_offset: 0,
            index_count: 6,
        },
        instance_buffer: vtx_buf,
        instance_offset: 0,
        uniforms: InlineUniforms::EMPTY,
        scissor: None,
    };
    gpu.encode(&DrawList {
        commands: &[cmd],
        passes: &[RenderPass {
            target: RenderTarget::Surface(frame),
            load: LoadOp::Clear([0.0, 0.0, 1.0, 1.0]),
            first_command: 0,
            command_count: 1,
        }],
    });
    gpu.present(frame);

    let center = gpu.surface_texel(surface, 32, 24);
    let outside = gpu.surface_texel(surface, 2, 2);
    (center, outside)
}

#[test]
fn u32_indexed_mesh_fills_the_quad() {
    let indices: [u32; 6] = [0, 1, 2, 0, 2, 3];
    let bytes: &[u8] = unsafe {
        core::slice::from_raw_parts(
            indices.as_ptr() as *const u8,
            core::mem::size_of_val(&indices),
        )
    };
    let (center, outside) = draw_indexed(IndexFormat::U32, bytes);
    assert!(
        center[1] > 0.99 && center[0] < 0.01 && center[2] < 0.01,
        "u32 mesh center should be green: {center:?}"
    );
    assert!(
        outside[2] > 0.99 && outside[0] < 0.01 && outside[1] < 0.01,
        "u32 mesh outside should be the blue clear: {outside:?}"
    );
}

#[test]
fn u16_indexed_mesh_fills_the_quad() {
    let indices: [u16; 6] = [0, 1, 2, 0, 2, 3];
    let bytes: &[u8] = unsafe {
        core::slice::from_raw_parts(
            indices.as_ptr() as *const u8,
            core::mem::size_of_val(&indices),
        )
    };
    let (center, outside) = draw_indexed(IndexFormat::U16, bytes);
    assert!(
        center[1] > 0.99 && center[0] < 0.01 && center[2] < 0.01,
        "u16 mesh center should be green: {center:?}"
    );
    assert!(
        outside[2] > 0.99 && outside[0] < 0.01 && outside[1] < 0.01,
        "u16 mesh outside should be the blue clear: {outside:?}"
    );
}
