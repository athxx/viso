//! The CPU raster draws a scene pass against its depth attachment: the nearer
//! of two overlapping instances shows whichever is drawn last, a back face is
//! culled, without a depth test the later draw wins, and a pass clears what
//! its target held.

use viso_gpu::backend::{
    DepthAttachment, DrawCommand, DrawList, Geometry, IndexFormat, InlineUniforms, RenderPass,
    RenderTarget,
};
use viso_gpu::scene::{SceneInstance, SceneUniforms, SceneVertex};
use viso_gpu::{
    BindGroupDesc, Binding, BlendMode, BufferDesc, BufferUsage, BuiltinShader, CpuRaster, CullMode,
    DepthState, GpuBackend, GpuPod, LoadOp, PipelineDesc, RawWindowHandle, TextureDesc,
    TextureFormat,
};

#[repr(C)]
#[derive(Clone, Copy, GpuPod)]
struct Vertex {
    position: [f32; 3],
    normal: [f32; 3],
    uv: [f32; 2],
}

const SIZE: u32 = 32;

/// A unit quad in the `xy` plane facing `+z`, two counter-clockwise
/// triangles.
fn quad() -> (Vec<u8>, Vec<u8>) {
    let corner = |x: f32, y: f32| SceneVertex {
        position: [x, y, 0.0],
        normal: [0.0, 0.0, 1.0],
        uv: [0.0; 2],
    };
    let verts = [
        corner(-1.0, -1.0),
        corner(1.0, -1.0),
        corner(1.0, 1.0),
        corner(-1.0, 1.0),
    ];
    let mut vb = vec![0u8; SceneVertex::SIZE * 4];
    for (v, out) in verts
        .iter()
        .zip(vb.as_chunks_mut::<{ SceneVertex::SIZE }>().0)
    {
        v.write_to(out);
    }
    let ib = [0u16, 1, 2, 0, 2, 3]
        .iter()
        .flat_map(|i| i.to_le_bytes())
        .collect();
    (vb, ib)
}

/// An instance at depth `z` of `color`, mirrored in `x` when `back`, so its
/// triangles wind clockwise toward the camera.
fn at(z: f32, color: [f32; 4], back: bool) -> SceneInstance {
    let sx = if back { -1.0 } else { 1.0 };
    SceneInstance {
        rows: [
            [sx, 0.0, 0.0, 0.0],
            [0.0, 1.0, 0.0, 0.0],
            [0.0, 0.0, 1.0, z],
        ],
        color,
    }
}

/// A reversed-Z perspective camera at the origin looking down `-z`.
fn uniforms() -> SceneUniforms {
    let (f, near) = (1.0 / (30f32.to_radians()).tan(), 0.1);
    SceneUniforms {
        view_proj: [
            f, 0.0, 0.0, 0.0, //
            0.0, f, 0.0, 0.0, //
            0.0, 0.0, 0.0, -1.0, //
            0.0, 0.0, near, 0.0,
        ],
        camera: [0.0; 4],
        light_dir: [0.0, 0.0, 1.0, 1.0],
        light_color: [1.0; 4],
        ..SceneUniforms::default()
    }
}

const RED: [f32; 4] = [1.0, 0.0, 0.0, 1.0];
const GREEN: [f32; 4] = [0.0, 1.0, 0.0, 1.0];

/// The center texel after drawing `instances` in order, one draw each.
fn draw(instances: &[SceneInstance], depth: Option<DepthState>, cull: CullMode) -> [f32; 4] {
    drawn(instances, depth, cull, false)
}

/// The center texel after drawing `instances`, and when `then_clear` a pass
/// that draws nothing over them.
fn drawn(
    instances: &[SceneInstance],
    depth: Option<DepthState>,
    cull: CullMode,
    then_clear: bool,
) -> [f32; 4] {
    let mut gpu = CpuRaster::new();
    let target = gpu.create_texture(&TextureDesc {
        width: SIZE,
        height: SIZE,
        format: TextureFormat::Rgba8Unorm,
        render_target: true,
        label: "scene",
    });
    let depth_texture = gpu.create_texture(&TextureDesc {
        width: SIZE,
        height: SIZE,
        format: TextureFormat::Depth32Float,
        render_target: true,
        label: "scene-depth",
    });
    let buffer = |gpu: &mut CpuRaster, bytes: &[u8], usage| {
        let b = gpu.create_buffer(&BufferDesc {
            size: bytes.len(),
            usage: usage | BufferUsage::CPU_WRITE,
            label: "scene",
        });
        gpu.write_buffer(b, 0, bytes);
        b
    };
    let (vb, ib) = quad();
    let vertices = buffer(&mut gpu, &vb, BufferUsage::VERTEX);
    let indices = buffer(&mut gpu, &ib, BufferUsage::INDEX);
    let mut ub = [0u8; SceneUniforms::SIZE];
    uniforms().write_to(&mut ub);
    let uniform = buffer(&mut gpu, &ub, BufferUsage::UNIFORM);
    let mut instance_bytes = vec![0u8; SceneInstance::SIZE * instances.len()];
    for (i, out) in instances
        .iter()
        .zip(instance_bytes.as_chunks_mut::<{ SceneInstance::SIZE }>().0)
    {
        i.write_to(out);
    }
    let instance_buffer = buffer(&mut gpu, &instance_bytes, BufferUsage::INSTANCE);
    let group = gpu.create_bind_group(&BindGroupDesc {
        label: "scene",
        bindings: vec![Binding::Uniform(uniform)],
    });
    let pipeline = gpu
        .create_pipeline(
            &PipelineDesc {
                label: "scene",
                builtin: BuiltinShader::Scene,
                variant: 0,
                code: viso_gpu::ShaderCode::None,
                vertex_entry: "vertex_main",
                fragment_entry: "fragment_main",
                color_format: TextureFormat::Rgba8Unorm,
                depth,
                cull,
                blend: BlendMode::Replace,
                instance_schema: viso_gpu::scene::VERTEX_SCHEMA,
            },
            &Vertex::LAYOUT,
        )
        .expect("the vertex layout matches");
    let commands: Vec<DrawCommand> = (0..instances.len())
        .map(|i| DrawCommand {
            pipeline,
            bind_group: Some(group),
            geometry: Geometry::InstancedMesh {
                vertex_buffer: vertices,
                index_buffer: indices,
                index_format: IndexFormat::U16,
                index_offset: 0,
                index_count: 6,
                count: 1,
            },
            instance_buffer,
            instance_offset: i * SceneInstance::SIZE,
            uniforms: InlineUniforms::EMPTY,
            scissor: None,
        })
        .collect();
    let surface = gpu.create_surface(RawWindowHandle::Headless, 1, 1);
    let frame = gpu.begin_frame(surface).expect("headless acquires");
    let pass = |first_command, command_count| RenderPass {
        target: RenderTarget::Texture(target),
        load: LoadOp::Clear([0.0, 0.0, 1.0, 1.0]),
        depth: Some(DepthAttachment {
            texture: depth_texture,
            clear: 0.0,
        }),
        first_command,
        command_count,
    };
    gpu.encode(&DrawList {
        commands: &commands,
        passes: &[pass(0, commands.len() as u32)],
    });
    if then_clear {
        gpu.encode(&DrawList {
            commands: &commands,
            passes: &[pass(0, 0)],
        });
    }
    gpu.present(frame);
    // The readback is BGRA.
    let bytes = gpu.read_texture(target);
    let at = ((SIZE / 2 * SIZE + SIZE / 2) * 4) as usize;
    [2, 1, 0, 3].map(|c| f32::from(bytes[at + c]) / 255.0)
}

#[test]
fn the_nearer_instance_shows_in_either_draw_order() {
    let near = at(-2.0, RED, false);
    let far = at(-4.0, GREEN, false);
    for order in [[near, far], [far, near]] {
        let c = draw(&order, Some(DepthState::REVERSED), CullMode::Back);
        assert!(c[0] > 0.9 && c[1] < c[0], "{c:?}");
    }
}

#[test]
fn without_a_depth_test_the_later_draw_wins() {
    let c = draw(
        &[at(-2.0, RED, false), at(-4.0, GREEN, false)],
        None,
        CullMode::None,
    );
    assert!(c[1] > 0.9 && c[0] < c[1], "{c:?}");
}

#[test]
fn a_scene_pass_clears_what_its_target_held() {
    let c = drawn(
        &[at(-2.0, RED, false)],
        Some(DepthState::REVERSED),
        CullMode::Back,
        true,
    );
    assert!(
        c[2] > 0.9 && c[0] < 0.1,
        "the clear replaced the drawn quad: {c:?}"
    );
}

#[test]
fn a_back_face_is_culled() {
    let c = draw(
        &[at(-2.0, RED, true)],
        Some(DepthState::REVERSED),
        CullMode::Back,
    );
    assert!(c[2] > 0.9 && c[0] < 0.1, "the blue clear shows: {c:?}");
    let c = draw(
        &[at(-2.0, RED, true)],
        Some(DepthState::REVERSED),
        CullMode::None,
    );
    // Its normal turns away from the light with its winding: drawn unlit.
    assert!(c[2] < 0.1, "drawn when nothing is culled: {c:?}");
}
