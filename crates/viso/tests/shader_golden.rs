//! `.vs` shaders drawn from the same encoded buffers by the reference
//! interpreter's rasterizer on the CPU, by Metal from the MSL, and — with the
//! `vulkan` feature, on MoltenVK — by Vulkan from the SPIR-V of the WGSL. Every
//! GPU image agrees with the reference, and Vulkan with Metal, within a small
//! per-channel tolerance.

#![cfg(target_os = "macos")]

use viso_dsl::frontend::{Origin, compile_file};
use viso_gpu::{
    AddressMode, BindGroupDesc, Binding, BlendMode, BufferDesc, BufferUsage, DrawCommand, DrawList,
    FilterMode, Geometry, GpuBackend, InlineUniforms, LoadOp, MetalBackend, PipelineId,
    ProgramDesc, RenderPass, RenderTarget, SamplerDesc, TextureDesc, TextureFormat, TextureId,
};
use viso_shader::program::{
    Bindings, FRAGMENT_ENTRY, Program, Raster, ShaderInterface, Target, TextureData, VERTEX_ENTRY,
    Value, emit,
};

const W: u32 = 64;
const H: u32 = 64;
const CLEAR: [f32; 4] = [0.05, 0.1, 0.15, 1.0];
/// Largest per-channel difference allowed, in 8-bit steps.
const TOLERANCE: u8 = 2;

fn program(source: &str) -> Program {
    let origin = Origin {
        package: "app".to_owned(),
        module: vec!["main".to_owned()],
        language: None,
    };
    let compiled = compile_file(source, &origin);
    let errors: Vec<_> = compiled.errors().collect();
    assert!(errors.is_empty(), "{errors:#?}");
    let shader = compiled.shaders.into_iter().next().expect("one shader");
    shader.program.expect("a program")
}

/// A 4×4 RGBA8 pattern, row-major from the top.
fn pattern() -> Vec<u8> {
    (0..16u8)
        .flat_map(|i| [i * 16, 255 - i * 16, (i % 4) * 80, 255])
        .collect()
}

struct Scene<'a> {
    source: &'a str,
    uniforms: Vec<Value>,
    instances: Vec<Vec<Value>>,
    texture: bool,
}

/// The pictures of one scene: the CPU reference's, Metal's, and Vulkan's
/// when a Vulkan device exists; all BGRA8, top-left.
struct Pictures {
    cpu: Vec<u8>,
    metal: Vec<u8>,
    vulkan: Option<Vec<u8>>,
}

/// What every backend draws from: the program, its interface, and the
/// encoded uniform and instance bytes.
struct Encoded {
    program: Program,
    interface: ShaderInterface,
    uniforms: Vec<u8>,
    instances: Vec<u8>,
    count: u32,
    texture: bool,
}

/// Renders `scene` on the CPU and on every GPU backend from the same bytes.
fn render(scene: &Scene<'_>) -> Pictures {
    let p = program(scene.source);
    let interface = p.interface();
    let uniforms = interface
        .uniforms
        .encode(&p.uniforms, &scene.uniforms)
        .expect("uniforms encode");
    let instances: Vec<u8> = scene
        .instances
        .iter()
        .flat_map(|row| {
            interface
                .instance
                .encode(&p.instance, row)
                .expect("instance encodes")
        })
        .collect();
    let encoded = Encoded {
        program: p,
        interface,
        uniforms,
        instances,
        count: scene.instances.len() as u32,
        texture: scene.texture,
    };
    Pictures {
        cpu: cpu(&encoded),
        metal: metal(&encoded),
        vulkan: vulkan(&encoded),
    }
}

/// The CPU reads what the GPU reads: the same bytes, decoded.
fn cpu(e: &Encoded) -> Vec<u8> {
    let p = &e.program;
    let uniforms = e.interface.uniforms.decode(p, &p.uniforms, &e.uniforms);
    let stride = e.interface.instance.size as usize;
    let instances: Vec<Vec<Value>> = e
        .instances
        .chunks_exact(stride)
        .map(|row| e.interface.instance.decode(p, &p.instance, row))
        .collect();
    let texels: Vec<[f32; 4]> = pattern()
        .as_chunks::<4>()
        .0
        .iter()
        .map(|t| std::array::from_fn(|i| f32::from(t[i]) / 255.0))
        .collect();
    let textures = [TextureData {
        width: 4,
        height: 4,
        texels,
    }];
    let samplers = [SAMPLER];
    let bindings = Bindings {
        uniforms: &uniforms,
        textures: if e.texture { &textures } else { &[] },
        samplers: if e.texture { &samplers } else { &[] },
    };
    let mut raster = Raster::new(W, H, CLEAR);
    raster.draw(p, &bindings, &instances, BlendMode::PremultipliedOver);
    raster.to_bgra8()
}

const SAMPLER: SamplerDesc = SamplerDesc {
    filter: FilterMode::Nearest,
    address: AddressMode::ClampToEdge,
};

fn metal(e: &Encoded) -> Vec<u8> {
    let p = &e.program;
    let mut gpu = MetalBackend::new();
    let msl = emit(p, &e.interface, Target::Msl);
    let compiled = gpu
        .compiler()
        .compile(&ProgramDesc {
            label: &p.name,
            msl: &msl,
            vertex_entry: VERTEX_ENTRY,
            fragment_entry: FRAGMENT_ENTRY,
            color_format: TextureFormat::Bgra8Unorm,
            blend: BlendMode::PremultipliedOver,
            vertex_textures: e.interface.vertex_textures,
        })
        .unwrap_or_else(|log| panic!("{log}\n{msl}"));
    let pipeline = gpu.install_program(&compiled);
    let target = draw(&mut gpu, pipeline, e);
    gpu.read_texture(target)
}

/// The program on Vulkan from the SPIR-V naga writes for its WGSL, or `None`
/// without a Vulkan device (or without the `vulkan` feature).
#[cfg(feature = "vulkan")]
fn vulkan(e: &Encoded) -> Option<Vec<u8>> {
    use viso_gpu::{VulkanBackend, VulkanProgramDesc};
    let Some(mut gpu) = VulkanBackend::try_new_with(true) else {
        eprintln!("no Vulkan device; Vulkan skipped");
        return None;
    };
    let p = &e.program;
    let wgsl = emit(p, &e.interface, Target::Wgsl);
    let module = naga::front::wgsl::parse_str(&wgsl).unwrap_or_else(|err| panic!("{err}\n{wgsl}"));
    let info = naga::valid::Validator::new(
        naga::valid::ValidationFlags::all(),
        naga::valid::Capabilities::empty(),
    )
    .validate(&module)
    .unwrap_or_else(|err| panic!("{err:?}\n{wgsl}"));
    let options = naga::back::spv::Options {
        lang_version: (1, 0),
        // Clip-space y flips in the vertex stage, as in the built-ins.
        flags: naga::back::spv::WriterFlags::ADJUST_COORDINATE_SPACE,
        // Every loop of a program is bounded by construction.
        force_loop_bounding: false,
        ..Default::default()
    };
    let words = naga::back::spv::write_vec(&module, &info, &options, None).expect("SPIR-V");
    let spirv: Vec<u8> = words.iter().flat_map(|w| w.to_le_bytes()).collect();
    let pipeline = gpu
        .install_program(&VulkanProgramDesc {
            label: &p.name,
            spirv: &spirv,
            vertex_entry: VERTEX_ENTRY,
            fragment_entry: FRAGMENT_ENTRY,
            color_format: TextureFormat::Bgra8Unorm,
            blend: BlendMode::PremultipliedOver,
            textures: e.interface.textures.len() as u32,
            samplers: e.interface.samplers.len() as u32,
        })
        .unwrap_or_else(|log| panic!("{log}"));
    let target = draw(&mut gpu, pipeline, e);
    let picture = gpu.read_texture(target);
    assert_eq!(
        gpu.validation_errors(),
        0,
        "the validation layer reported errors"
    );
    eprintln!(
        "Vulkan validation layer {}",
        if gpu.validation_enabled() {
            "on"
        } else {
            "off"
        }
    );
    Some(picture)
}

#[cfg(not(feature = "vulkan"))]
fn vulkan(_: &Encoded) -> Option<Vec<u8>> {
    None
}

/// Uploads `e`'s bytes to `gpu` and draws them with `pipeline` into a fresh
/// target, which it returns.
fn draw<B: GpuBackend>(gpu: &mut B, pipeline: PipelineId, e: &Encoded) -> TextureId {
    let mut buffer = |bytes: &[u8], usage| {
        let id = gpu.create_buffer(&BufferDesc {
            size: bytes.len().max(16),
            usage,
            label: "golden",
        });
        gpu.write_buffer(id, 0, bytes);
        id
    };
    let uniform_buffer = buffer(&e.uniforms, BufferUsage::UNIFORM);
    let instance_buffer = buffer(&e.instances, BufferUsage::INSTANCE);
    let mut binds = vec![Binding::Uniform(uniform_buffer)];
    if e.texture {
        let texture = gpu.create_texture(&TextureDesc {
            width: 4,
            height: 4,
            format: TextureFormat::Rgba8Unorm,
            render_target: false,
            label: "pattern",
        });
        gpu.write_texture(texture, 0, 0, 4, 4, &pattern());
        binds.push(Binding::Texture(texture));
        binds.push(Binding::Sampler(gpu.create_sampler(&SAMPLER)));
    }
    let bind_group = gpu.create_bind_group(&BindGroupDesc {
        label: "golden",
        bindings: binds,
    });
    let target = gpu.create_texture(&TextureDesc {
        width: W,
        height: H,
        format: TextureFormat::Bgra8Unorm,
        render_target: true,
        label: "golden-target",
    });
    let commands = [DrawCommand {
        pipeline,
        bind_group: Some(bind_group),
        geometry: Geometry::Generated { count: e.count },
        instance_buffer,
        instance_offset: 0,
        uniforms: InlineUniforms::new(&[]),
        scissor: None,
    }];
    let passes = [RenderPass {
        target: RenderTarget::Texture(target),
        load: LoadOp::Clear(CLEAR),
        first_command: 0,
        command_count: 1,
    }];
    gpu.encode(&DrawList {
        commands: &commands,
        passes: &passes,
    });
    target
}

/// Each GPU picture against the CPU reference, and Vulkan against Metal.
fn compare_all(name: &str, pictures: &Pictures) {
    compare(&format!("{name} Metal"), &pictures.metal, &pictures.cpu);
    if let Some(vulkan) = &pictures.vulkan {
        compare(&format!("{name} Vulkan"), vulkan, &pictures.cpu);
        compare(&format!("{name} Vulkan/Metal"), vulkan, &pictures.metal);
    }
}

/// The largest per-channel difference and the first pixel over tolerance.
fn compare(name: &str, gpu: &[u8], cpu: &[u8]) {
    assert_eq!(gpu.len(), cpu.len());
    let mut worst = 0;
    let mut over = Vec::new();
    for (i, (g, c)) in gpu
        .as_chunks::<4>()
        .0
        .iter()
        .zip(cpu.as_chunks::<4>().0.iter())
        .enumerate()
    {
        let d = g
            .iter()
            .zip(c)
            .map(|(g, c)| g.abs_diff(*c))
            .max()
            .unwrap_or(0);
        worst = worst.max(d);
        if d > TOLERANCE {
            over.push((i as u32 % W, i as u32 / W, g.to_vec(), c.to_vec()));
        }
    }
    eprintln!("{name}: worst channel difference {worst}");
    assert!(
        over.is_empty(),
        "{name}: {} pixels over tolerance, first {:?}",
        over.len(),
        over.first()
    );
    // The picture is not trivially the clear color.
    let clear = Raster::new(1, 1, CLEAR).to_bgra8();
    let drawn = cpu
        .as_chunks::<4>()
        .0
        .iter()
        .filter(|p| *p != clear.as_slice())
        .count();
    assert!(
        drawn > (W * H / 8) as usize,
        "{name}: only {drawn} pixels drawn"
    );
    let mut colors: Vec<&[u8; 4]> = cpu.as_chunks::<4>().0.iter().collect();
    colors.sort_unstable();
    colors.dedup();
    eprintln!(
        "{name}: {drawn} pixels drawn, {} distinct colors",
        colors.len()
    );
    assert!(
        colors.len() > 32,
        "{name}: only {} distinct colors",
        colors.len()
    );
}

#[test]
fn the_spec_example_matches_the_reference() {
    let source = r#"
export shader RoundedRect {
    uniform viewport_size: Vec2F32;
    instance rect_pos: Vec2F32;
    instance rect_size: Vec2F32;
    instance radius: F32;
    instance color: ColorLinear;

    varying local_pos: Vec2F32;

    vertex(vertex_id: U32) -> VertexOutput {
        let unit = quad_vertex(vertex_id);
        let position = rect_pos + unit * rect_size;
        local_pos = unit * rect_size;

        return VertexOutput {
            clip_position: to_clip(position, viewport_size),
        };
    }

    fragment() -> Vec4F32 {
        let distance = rounded_rect_sdf(local_pos, rect_size, radius);
        let alpha = smoothstep(1.0f32, 0.0f32, distance);
        return color.to_vec4() * alpha;
    }
}
"#;
    let rect = |x: f32, y: f32, w: f32, h: f32, r: f32, c: [f32; 4]| {
        vec![
            Value::floats(&[x, y]),
            Value::floats(&[w, h]),
            Value::f32(r),
            Value::floats(&c),
        ]
    };
    let scene = Scene {
        source,
        uniforms: vec![Value::floats(&[W as f32, H as f32])],
        instances: vec![
            rect(4.0, 4.0, 40.0, 28.0, 8.0, [0.8, 0.2, 0.1, 1.0]),
            rect(20.0, 16.0, 40.0, 44.0, 14.0, [0.1, 0.3, 0.6, 0.75]),
            rect(8.0, 40.0, 20.0, 20.0, 30.0, [0.5, 0.5, 0.0, 0.5]),
        ],
        texture: false,
    };
    compare_all("RoundedRect", &render(&scene));
}

#[test]
fn records_matrices_loops_textures_and_discard_match_the_reference() {
    let source = r#"
@shader_value
record Light { dir: Vec3F32; strength: F32; }

shader Fancy {
    uniform viewport: Vec2F32;
    uniform basis: Mat2F32;
    uniform light: Light;
    uniform mode: I32;
    uniform rings: I32;
    instance pos: Vec2F32;
    instance size: Vec2F32;
    instance tint: ColorLinear;
    instance seed: U32;
    texture pattern: Texture2D<Vec4F32>;
    sampler nearest: Sampler;
    varying local: Vec2F32;
    varying uv: Vec2F32;
    varying id: U32;

    fn shade(n: Vec3F32, l: Light) -> F32 {
        let lit = dot(normalize(n), normalize(l.dir));
        if lit < 0.0 { return 0.0; }
        return clamp(lit, 0.0, 1.0) * l.strength;
    }

    fn bands(d: F32, count: I32) -> F32 {
        let mut ring = 0.0;
        @max_iterations(8)
        for i in 0..count {
            if i == 1 { continue; }
            if i > 4 { break; }
            let edge = (i as F32) * 5.0;
            ring += smoothstep(edge, edge + 3.0, d) * 0.125;
        }
        return ring;
    }

    vertex(vertex_id: U32, instance_id: U32) -> VertexOutput {
        let unit = quad_vertex(vertex_id);
        local = unit * size;
        uv = unit;
        id = seed + instance_id;
        return VertexOutput { clip_position: to_clip(pos + local, viewport) };
    }

    fragment(frag_coord: Vec4F32) -> Vec4F32 {
        let texel = sample(pattern, nearest, uv);
        let center = size * 0.5;
        let d = distance(local, center);
        let cell = ((frag_coord.x as U32) >> 2u32) + ((frag_coord.y as U32) >> 2u32);
        if id == 7u32 && (cell & 1u32) == 1u32 { discard(); }
        let k = match mode { 0 => 0.25, 1 => 0.5, _ => 1.0 };
        let n = Vec3F32(basis * (uv * 2.0 - 1.0), 1.0);
        let lit = shade(n, light);
        let edge = clamp(local.x / (fwidth(local.x) * 4.0), 0.0, 1.0);
        let bits = ((id << 2u32) | (id >> 1u32)) ^ 5u32;
        let tag = ((bits & 7u32) as F32) * 0.05;
        let ring = bands(d, rings);
        let m = Mat2F32(Vec2F32(1.0, 0.0), Vec2F32(0.0, 1.0)) * transpose(basis);
        let swirl = (m * Vec2F32(1.0, 0.0)).x * 0.1;
        let rgb = (texel.rgb * 0.5 + tint.to_vec4().rgb * (lit * k + ring)) * edge + Vec3F32(tag, swirl, 0.0);
        let alpha = tint.a;
        return Vec4F32(clamp(rgb, 0.0, 1.0) * alpha, alpha);
    }
}
"#;
    let (c, s) = (0.5f32.cos(), 0.5f32.sin());
    let row = |x: f32, y: f32, w: f32, h: f32, tint: [f32; 4], seed: u32| {
        vec![
            Value::floats(&[x, y]),
            Value::floats(&[w, h]),
            Value::floats(&tint),
            Value::u32(seed),
        ]
    };
    let scene = Scene {
        source,
        uniforms: vec![
            Value::floats(&[W as f32, H as f32]),
            Value::Matrix(2, [[c, s, 0.0, 0.0], [-s, c, 0.0, 0.0], [0.0; 4], [0.0; 4]]),
            Value::Record(vec![Value::floats(&[0.3, -0.4, 1.0]), Value::f32(0.9)]),
            Value::i32(1),
            Value::i32(6),
        ],
        instances: vec![
            row(0.0, 0.0, 48.0, 48.0, [0.9, 0.6, 0.2, 1.0], 3),
            row(16.0, 16.0, 48.0, 48.0, [0.2, 0.7, 0.9, 0.8], 6),
        ],
        texture: true,
    };
    compare_all("Fancy", &render(&scene));
}
