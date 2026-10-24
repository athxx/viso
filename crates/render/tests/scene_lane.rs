//! The scene built-in draws the same lit, depth-tested image on every backend
//! that runs here.
//!
//! A ground slab, a tilted box in front of a second box and a squashed box
//! are drawn as instanced meshes into a depth pass, nearest first so the depth
//! test decides what shows. The CPU raster's image is the reference
//! (`SceneUniforms::shade`, the expression every scene program computes). On
//! an Apple device the native MSL draws it too, and so does the web WGSL
//! translated to MSL by `naga`, which checks the WGSL lowering the Vulkan and
//! WebGPU backends run.

use viso_gpu::backend::{
    DepthAttachment, DrawCommand, DrawList, Geometry, IndexFormat, InlineUniforms, RenderPass,
    RenderTarget,
};
use viso_gpu::scene::{self, SceneInstance, SceneUniforms, SceneVertex};
use viso_gpu::{
    BindGroupDesc, Binding, BlendMode, BufferDesc, BufferUsage, BuiltinShader, CpuRaster, CullMode,
    DepthState, GpuBackend, GpuPod, LoadOp, PipelineDesc, ShaderCode, TextureDesc, TextureFormat,
};

#[repr(C)]
#[derive(Clone, Copy, GpuPod)]
struct Vertex {
    position: [f32; 3],
    normal: [f32; 3],
    uv: [f32; 2],
}

const W: u32 = 96;
const H: u32 = 64;

/// A unit cube, `[-1, 1]` on each axis: four vertices per face with its
/// normal, two triangles counter-clockwise seen from outside.
fn cube() -> (Vec<SceneVertex>, Vec<u16>) {
    // Each face's normal and two in-plane axes with `u × v = n`.
    let faces: [([f32; 3], [f32; 3], [f32; 3]); 6] = [
        ([1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]),
        ([-1.0, 0.0, 0.0], [0.0, 0.0, 1.0], [0.0, 1.0, 0.0]),
        ([0.0, 1.0, 0.0], [0.0, 0.0, 1.0], [1.0, 0.0, 0.0]),
        ([0.0, -1.0, 0.0], [1.0, 0.0, 0.0], [0.0, 0.0, 1.0]),
        ([0.0, 0.0, 1.0], [1.0, 0.0, 0.0], [0.0, 1.0, 0.0]),
        ([0.0, 0.0, -1.0], [0.0, 1.0, 0.0], [1.0, 0.0, 0.0]),
    ];
    let mut vertices = Vec::new();
    let mut indices = Vec::new();
    for (n, u, v) in faces {
        let base = vertices.len() as u16;
        for (su, sv) in [(-1.0, -1.0), (1.0, -1.0), (1.0, 1.0), (-1.0, 1.0)] {
            vertices.push(SceneVertex {
                position: core::array::from_fn(|i| n[i] + su * u[i] + sv * v[i]),
                normal: n,
                uv: [su * 0.5 + 0.5, sv * 0.5 + 0.5],
            });
        }
        indices.extend([0, 1, 2, 0, 2, 3].map(|k| base + k));
    }
    (vertices, indices)
}

/// A box at `at` with half extents `half`, turned `yaw` radians about y.
fn placed(at: [f32; 3], half: [f32; 3], yaw: f32, color: [f32; 4]) -> SceneInstance {
    let (s, c) = yaw.sin_cos();
    SceneInstance {
        rows: [
            [c * half[0], 0.0, s * half[2], at[0]],
            [0.0, half[1], 0.0, at[1]],
            [-s * half[0], 0.0, c * half[2], at[2]],
        ],
        color,
    }
}

/// `a * b` for column-major 4×4 matrices.
fn mul(a: &[f32; 16], b: &[f32; 16]) -> [f32; 16] {
    core::array::from_fn(|i| {
        let (col, row) = (i / 4, i % 4);
        (0..4).map(|k| a[k * 4 + row] * b[col * 4 + k]).sum()
    })
}

/// A right-handed camera at `eye` looking at the origin, with a reversed-Z
/// perspective projection.
fn uniforms() -> SceneUniforms {
    let eye = [3.5f32, 3.0, 5.5];
    let f = scene::normalize([-eye[0], -eye[1], -eye[2]]);
    let cross = |a: [f32; 3], b: [f32; 3]| {
        [
            a[1] * b[2] - a[2] * b[1],
            a[2] * b[0] - a[0] * b[2],
            a[0] * b[1] - a[1] * b[0],
        ]
    };
    let dot = |a: [f32; 3], b: [f32; 3]| a[0] * b[0] + a[1] * b[1] + a[2] * b[2];
    let s = scene::normalize(cross(f, [0.0, 1.0, 0.0]));
    let u = cross(s, f);
    let view = [
        s[0],
        u[0],
        -f[0],
        0.0,
        s[1],
        u[1],
        -f[1],
        0.0,
        s[2],
        u[2],
        -f[2],
        0.0,
        -dot(s, eye),
        -dot(u, eye),
        dot(f, eye),
        1.0,
    ];
    let (near, far) = (0.5f32, 50.0f32);
    let fy = 1.0 / (25f32.to_radians()).tan();
    let fx = fy * H as f32 / W as f32;
    let proj = [
        fx,
        0.0,
        0.0,
        0.0,
        0.0,
        fy,
        0.0,
        0.0,
        0.0,
        0.0,
        near / (far - near),
        -1.0,
        0.0,
        0.0,
        near * far / (far - near),
        0.0,
    ];
    let light = scene::normalize([0.4, 0.8, 0.3]);
    SceneUniforms {
        view_proj: mul(&proj, &view),
        camera: [eye[0], eye[1], eye[2], 0.0],
        light_dir: [light[0], light[1], light[2], 1.0],
        light_color: [1.0, 0.95, 0.9, 0.0],
        sky: [0.3, 0.35, 0.45, 0.0],
        ground: [0.08, 0.07, 0.06, 0.0],
        fog: [0.6, 0.7, 0.8, 0.03],
    }
}

/// The instances, nearest first.
fn instances() -> Vec<SceneInstance> {
    vec![
        placed([0.6, 0.0, 1.2], [0.7, 0.7, 0.7], 0.5, [0.9, 0.25, 0.2, 1.0]),
        placed(
            [-1.0, 0.2, -0.8],
            [0.9, 0.9, 0.9],
            -0.3,
            [0.2, 0.6, 0.9, 1.0],
        ),
        placed(
            [1.8, -0.4, -1.5],
            [0.8, 0.3, 0.5],
            1.1,
            [0.95, 0.85, 0.3, 1.0],
        ),
        placed(
            [0.0, -1.0, 0.0],
            [4.0, 0.3, 4.0],
            0.0,
            [0.4, 0.6, 0.35, 1.0],
        ),
    ]
}

fn bytes<T>(items: &[T], write: impl Fn(&T, &mut [u8]), size: usize) -> Vec<u8> {
    let mut out = vec![0u8; items.len() * size];
    for (item, chunk) in items.iter().zip(out.chunks_exact_mut(size)) {
        write(item, chunk);
    }
    out
}

/// The scene drawn by `gpu` with the scene program `code`, as BGRA8 bytes
/// read back by `read`.
fn draw<B: GpuBackend>(
    gpu: &mut B,
    code: ShaderCode,
    read: impl Fn(&mut B, viso_gpu::TextureId) -> Vec<u8>,
) -> Vec<u8> {
    let target = gpu.create_texture(&TextureDesc {
        width: W,
        height: H,
        format: TextureFormat::Bgra8Unorm,
        render_target: true,
        label: "scene",
    });
    let depth = gpu.create_texture(&TextureDesc {
        width: W,
        height: H,
        format: TextureFormat::Depth32Float,
        render_target: true,
        label: "scene-depth",
    });
    let buffer = |gpu: &mut B, bytes: &[u8], usage| {
        let b = gpu.create_buffer(&BufferDesc {
            size: bytes.len(),
            usage: usage | BufferUsage::CPU_WRITE,
            label: "scene",
        });
        gpu.write_buffer(b, 0, bytes);
        b
    };
    let (vertices, indices) = cube();
    let vertex_bytes = bytes(&vertices, SceneVertex::write_to, SceneVertex::SIZE);
    let index_bytes: Vec<u8> = indices.iter().flat_map(|i| i.to_le_bytes()).collect();
    let instances = instances();
    let instance_bytes = bytes(&instances, SceneInstance::write_to, SceneInstance::SIZE);
    let mut uniform_bytes = [0u8; SceneUniforms::SIZE];
    uniforms().write_to(&mut uniform_bytes);

    let vertex_buffer = buffer(gpu, &vertex_bytes, BufferUsage::VERTEX);
    let index_buffer = buffer(gpu, &index_bytes, BufferUsage::INDEX);
    let instance_buffer = buffer(gpu, &instance_bytes, BufferUsage::INSTANCE);
    let uniform = buffer(gpu, &uniform_bytes, BufferUsage::UNIFORM);
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
                code,
                vertex_entry: "vertex_main",
                fragment_entry: "fragment_main",
                color_format: TextureFormat::Bgra8Unorm,
                depth: Some(DepthState::REVERSED),
                cull: CullMode::Back,
                blend: BlendMode::Replace,
                instance_schema: scene::VERTEX_SCHEMA,
            },
            &Vertex::LAYOUT,
        )
        .expect("the vertex layout matches");
    // One draw per instance, nearest first: the depth test hides what is
    // behind; and one draw of all of them, which must agree.
    let one = |i: usize, count: u32| DrawCommand {
        pipeline,
        bind_group: Some(group),
        geometry: Geometry::InstancedMesh {
            vertex_buffer,
            index_buffer,
            index_format: IndexFormat::U16,
            index_offset: 0,
            index_count: indices.len() as u32,
            count,
        },
        instance_buffer,
        instance_offset: i * SceneInstance::SIZE,
        uniforms: InlineUniforms::EMPTY,
        scissor: None,
    };
    let mut commands: Vec<DrawCommand> = (0..instances.len()).map(|i| one(i, 1)).collect();
    commands.push(one(0, instances.len() as u32));
    let pass = |first: u32, count: u32| RenderPass {
        target: RenderTarget::Texture(target),
        load: LoadOp::Clear([0.6, 0.7, 0.8, 1.0]),
        depth: Some(DepthAttachment {
            texture: depth,
            clear: 0.0,
        }),
        first_command: first,
        command_count: count,
    };
    gpu.encode(&DrawList {
        commands: &commands,
        passes: &[pass(0, instances.len() as u32)],
    });
    let separate = read(gpu, target);
    gpu.encode(&DrawList {
        commands: &commands,
        passes: &[pass(instances.len() as u32, 1)],
    });
    let together = read(gpu, target);
    let (off, worst) = compare(&separate, &together, 0);
    assert!(
        off == 0.0,
        "one instanced draw differs from one draw per instance: {:.2}%, by {worst}",
        off * 100.0
    );
    together
}

/// How far two BGRA8 images are apart: the share of pixels with a channel
/// more than `tolerance` off, and the worst channel difference.
fn compare(a: &[u8], b: &[u8], tolerance: u8) -> (f32, u8) {
    assert_eq!(a.len(), b.len());
    let mut off = 0;
    let mut worst = 0;
    for (pa, pb) in a.as_chunks::<4>().0.iter().zip(b.as_chunks::<4>().0) {
        let d = pa
            .iter()
            .zip(pb)
            .map(|(x, y)| x.abs_diff(*y))
            .max()
            .unwrap();
        worst = worst.max(d);
        if d > tolerance {
            off += 1;
        }
    }
    (off as f32 / (a.len() / 4) as f32, worst)
}

fn cpu_image() -> Vec<u8> {
    let mut gpu = CpuRaster::new();
    draw(&mut gpu, ShaderCode::None, |gpu, t| gpu.read_texture(t))
}

#[test]
fn the_cpu_raster_draws_the_lit_scene() {
    let image = cpu_image();
    let at = |x: u32, y: u32| {
        let i = ((y * W + x) * 4) as usize;
        [image[i + 2], image[i + 1], image[i]]
    };
    let clear = [153, 179, 204];
    let covered = (0..W * H).filter(|i| at(i % W, i / W) != clear).count();
    assert!(
        covered > (W * H / 3) as usize,
        "the boxes and ground cover the view"
    );
    // The red box faces the camera near the middle; its lit top is brighter
    // than its side.
    let shades: Vec<[u8; 3]> = (0..W * H)
        .map(|i| at(i % W, i / W))
        .filter(|c| c[0] > 120 && c[1] < 110 && c[2] < 110)
        .collect();
    assert!(shades.len() > 40, "the red box shows");
    let brightest = shades.iter().map(|c| c[0]).max().unwrap();
    let darkest = shades.iter().map(|c| c[0]).min().unwrap();
    assert!(brightest > darkest + 30, "its faces are lit differently");
}

#[cfg(target_vendor = "apple")]
mod naga_pull;

#[cfg(target_vendor = "apple")]
mod metal {
    use super::*;
    use std::collections::BTreeMap;

    use super::naga_pull;

    use naga::back::msl;
    use viso_gpu::{MetalBackend, ShaderLang};
    use viso_shader::{PrimitiveKind, shader_code};

    #[test]
    fn metal_draws_what_the_cpu_raster_draws() {
        let reference = cpu_image();
        let mut gpu = MetalBackend::new();
        let code = shader_code(PrimitiveKind::Scene, ShaderLang::Msl).unwrap();
        let image = draw(&mut gpu, code, |gpu, t| gpu.read_texture(t));
        let (off, worst) = compare(&reference, &image, 3);
        assert!(
            off < 0.01,
            "{:.2}% of pixels differ, worst by {worst}",
            off * 100.0
        );
    }

    /// The web WGSL, translated to MSL with vertex pulling over the Metal
    /// backend's argument table: vertices at buffer 0, instances at 1, the
    /// uniform buffer at 2.
    fn translated() -> String {
        let ShaderCode::Wgsl(wgsl) = shader_code(PrimitiveKind::Scene, ShaderLang::Wgsl).unwrap()
        else {
            unreachable!("a WGSL request yields WGSL");
        };
        let module = naga::front::wgsl::parse_str(wgsl).unwrap();
        let info = naga::valid::Validator::new(
            naga::valid::ValidationFlags::all(),
            naga::valid::Capabilities::all(),
        )
        .validate(&module)
        .unwrap();
        let mut resources = BTreeMap::new();
        resources.insert(
            naga::ResourceBinding {
                group: 0,
                binding: 0,
            },
            msl::BindTarget {
                buffer: Some(2),
                ..Default::default()
            },
        );
        let entry = msl::EntryPointResources {
            resources,
            immediates_buffer: None,
            sizes_buffer: Some(30),
        };
        let options = msl::Options {
            lang_version: (2, 3),
            per_entry_point_map: [
                ("vertex_main".to_owned(), entry.clone()),
                ("fragment_main".to_owned(), entry),
            ]
            .into(),
            fake_missing_bindings: false,
            ..Default::default()
        };
        let mapping = |schema: viso_gpu::InstanceSchema, first: u32| {
            let mut offset = 0;
            schema
                .attributes
                .iter()
                .enumerate()
                .map(|(i, a)| {
                    let format = match a.format.size() {
                        8 => naga_types::VertexFormat::Float32x2,
                        12 => naga_types::VertexFormat::Float32x3,
                        _ => naga_types::VertexFormat::Float32x4,
                    };
                    let m = msl::AttributeMapping {
                        shader_location: first + i as u32,
                        offset,
                        format,
                    };
                    offset += a.format.size() as u32;
                    m
                })
                .collect::<Vec<_>>()
        };
        let pipeline = msl::PipelineOptions {
            vertex_pulling_transform: true,
            vertex_buffer_mappings: vec![
                msl::VertexBufferMapping {
                    id: 0,
                    stride: SceneVertex::SIZE as u32,
                    step_mode: msl::VertexBufferStepMode::ByVertex,
                    attributes: mapping(scene::VERTEX_SCHEMA, 0),
                },
                msl::VertexBufferMapping {
                    id: 1,
                    stride: SceneInstance::SIZE as u32,
                    step_mode: msl::VertexBufferStepMode::ByInstance,
                    attributes: mapping(scene::INSTANCE_SCHEMA, 3),
                },
            ],
            ..Default::default()
        };
        let (source, _) = msl::write_string(&module, &info, &options, &pipeline).unwrap();
        naga_pull::strip_pull_bounds(&source)
    }

    #[test]
    fn the_translated_wgsl_draws_what_the_msl_draws() {
        let reference = cpu_image();
        let msl = Box::leak(translated().into_boxed_str());
        let mut gpu = MetalBackend::new();
        let image = draw(&mut gpu, ShaderCode::Msl(msl), |gpu, t| gpu.read_texture(t));
        let (off, worst) = compare(&reference, &image, 3);
        assert!(
            off < 0.01,
            "{:.2}% of pixels differ, worst by {worst}",
            off * 100.0
        );
    }
}
