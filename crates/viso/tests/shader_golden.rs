//! `.vs` shaders drawn twice from the same encoded buffers: by the reference
//! interpreter's rasterizer on the CPU and by Metal. The two images agree
//! within a small per-channel tolerance.

#![cfg(target_os = "macos")]

use viso_dsl::frontend::{Origin, compile_file};
use viso_gpu::{
    AddressMode, BindGroupDesc, Binding, BlendMode, BufferDesc, BufferUsage, DrawCommand, DrawList,
    FilterMode, Geometry, GpuBackend, InlineUniforms, LoadOp, MetalBackend, ProgramDesc,
    RenderPass, RenderTarget, SamplerDesc, TextureDesc, TextureFormat,
};
use viso_shader::program::{
    Bindings, FRAGMENT_ENTRY, Program, Raster, Target, TextureData, VERTEX_ENTRY, Value, emit,
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

/// Renders `scene` on Metal and on the CPU; both BGRA8, top-left.
fn render(scene: &Scene<'_>) -> (Vec<u8>, Vec<u8>) {
    let p = program(scene.source);
    let interface = p.interface();
    let uniform_bytes = interface
        .uniforms
        .encode(&p.uniforms, &scene.uniforms)
        .expect("uniforms encode");
    let instance_bytes: Vec<u8> = scene
        .instances
        .iter()
        .flat_map(|row| {
            interface
                .instance
                .encode(&p.instance, row)
                .expect("instance encodes")
        })
        .collect();

    // The CPU reads what the GPU reads: the same bytes, decoded.
    let uniforms = interface.uniforms.decode(&p, &p.uniforms, &uniform_bytes);
    let stride = interface.instance.size as usize;
    let instances: Vec<Vec<Value>> = instance_bytes
        .chunks_exact(stride)
        .map(|row| interface.instance.decode(&p, &p.instance, row))
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
    let sampler = SamplerDesc {
        filter: FilterMode::Nearest,
        address: AddressMode::ClampToEdge,
    };
    let samplers = [sampler];
    let bindings = Bindings {
        uniforms: &uniforms,
        textures: if scene.texture { &textures } else { &[] },
        samplers: if scene.texture { &samplers } else { &[] },
    };
    let mut raster = Raster::new(W, H, CLEAR);
    raster.draw(&p, &bindings, &instances, BlendMode::PremultipliedOver);

    let mut gpu = MetalBackend::new();
    let msl = emit(&p, &interface, Target::Msl);
    let compiled = gpu
        .compiler()
        .compile(&ProgramDesc {
            label: &p.name,
            msl: &msl,
            vertex_entry: VERTEX_ENTRY,
            fragment_entry: FRAGMENT_ENTRY,
            color_format: TextureFormat::Bgra8Unorm,
            blend: BlendMode::PremultipliedOver,
            vertex_textures: interface.vertex_textures,
        })
        .unwrap_or_else(|log| panic!("{log}\n{msl}"));
    let pipeline = gpu.install_program(&compiled);
    let buffer = |gpu: &mut MetalBackend, bytes: &[u8], usage| {
        let id = gpu.create_buffer(&BufferDesc {
            size: bytes.len().max(16),
            usage,
            label: "golden",
        });
        gpu.write_buffer(id, 0, bytes);
        id
    };
    let uniform_buffer = buffer(&mut gpu, &uniform_bytes, BufferUsage::UNIFORM);
    let instance_buffer = buffer(&mut gpu, &instance_bytes, BufferUsage::INSTANCE);
    let mut binds = vec![Binding::Uniform(uniform_buffer)];
    if scene.texture {
        let texture = gpu.create_texture(&TextureDesc {
            width: 4,
            height: 4,
            format: TextureFormat::Rgba8Unorm,
            render_target: false,
            label: "pattern",
        });
        gpu.write_texture(texture, 0, 0, 4, 4, &pattern());
        binds.push(Binding::Texture(texture));
        binds.push(Binding::Sampler(gpu.create_sampler(&sampler)));
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
        geometry: Geometry::Generated {
            count: scene.instances.len() as u32,
        },
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
    (gpu.read_texture(target), raster.to_bgra8())
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
    let (gpu, cpu) = render(&scene);
    compare("RoundedRect", &gpu, &cpu);
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
    let (gpu, cpu) = render(&scene);
    compare("Fancy", &gpu, &cpu);
}
