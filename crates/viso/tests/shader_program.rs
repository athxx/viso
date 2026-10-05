//! `.vs` shader declarations lowered to a `viso_shader` program, laid out and
//! emitted for every target: the generated source must build for every
//! construct the shader subset admits, and lay its buffers out as the
//! descriptors say.

#![cfg(target_os = "macos")]

use std::process::Command;
use viso_dsl::frontend::{Origin, compile_file};
use viso_gpu::{BlendMode, MetalBackend, ProgramDesc, TextureFormat};

use viso_shader::program::{FRAGMENT_ENTRY, Program, ShaderInterface, Target, VERTEX_ENTRY, emit};

fn program(source: &str) -> Program {
    let origin = Origin {
        package: "app".to_owned(),
        module: vec!["main".to_owned()],
        language: None,
    };
    let compiled = compile_file(source, &origin);
    let errors: Vec<_> = compiled.errors().collect();
    assert!(errors.is_empty(), "{errors:#?}");
    let [shader] = compiled.shaders.as_slice() else {
        panic!("one shader");
    };
    let program = shader.program.clone().expect("a program");
    program.validate().expect("valid");
    program
}

/// Emits `source`'s program for every target and builds each: Metal
/// compiles the MSL, naga validates the WGSL and reflects its layout against
/// the descriptors, glslang compiles the HLSL when it is installed.
fn compiles(source: &str) {
    let p = program(source);
    let interface = p.interface();
    metal(&p, &interface);
    wgsl(&p, &interface);
    hlsl(&p, &interface);
}

fn metal(p: &Program, interface: &ShaderInterface) {
    let msl = emit(p, interface, Target::Msl);
    if std::env::var_os("VISO_DUMP_SHADER").is_some() {
        eprintln!("{msl}");
    }
    let backend = MetalBackend::new();
    let desc = ProgramDesc {
        label: &p.name,
        msl: &msl,
        vertex_entry: VERTEX_ENTRY,
        fragment_entry: FRAGMENT_ENTRY,
        color_format: TextureFormat::Bgra8Unorm,
        blend: BlendMode::PremultipliedOver,
        vertex_textures: interface.vertex_textures,
    };
    if let Err(log) = backend.compiler().compile(&desc) {
        panic!("{log}\n--- MSL ---\n{msl}");
    }
}

fn wgsl(p: &Program, interface: &ShaderInterface) {
    let src = emit(p, interface, Target::Wgsl);
    if std::env::var_os("VISO_DUMP_SHADER").is_some() {
        eprintln!("{src}");
    }
    let module = naga::front::wgsl::parse_str(&src)
        .unwrap_or_else(|e| panic!("{}\n--- WGSL ---\n{src}", e.emit_to_string(&src)));
    let mut validator = naga::valid::Validator::new(
        naga::valid::ValidationFlags::all(),
        naga::valid::Capabilities::empty(),
    );
    if let Err(e) = validator.validate(&module) {
        panic!("{}\n--- WGSL ---\n{src}", e.emit_to_string(&src));
    }
    // The struct naga lays out is the descriptor, offset for offset.
    let reflect = |name: &str| {
        module.types.iter().find_map(|(_, t)| match &t.inner {
            naga::TypeInner::Struct { members, span } if t.name.as_deref() == Some(name) => {
                Some((members.iter().map(|m| m.offset).collect::<Vec<_>>(), *span))
            }
            _ => None,
        })
    };
    if !interface.uniforms.fields.is_empty() {
        let (offsets, span) = reflect("Uniforms").expect("a uniform struct");
        let want: Vec<u32> = interface.uniforms.fields.iter().map(|f| f.offset).collect();
        assert_eq!((offsets, span), (want, interface.uniforms.size), "{src}");
    }
    if !interface.instance.fields.is_empty() {
        let (_, span) = reflect("Instance").expect("an instance struct");
        assert_eq!(span, interface.instance.size, "{src}");
    }
}

fn hlsl(p: &Program, interface: &ShaderInterface) {
    let src = emit(p, interface, Target::Hlsl);
    if std::env::var_os("VISO_DUMP_SHADER").is_some() {
        eprintln!("{src}");
    }
    let dir = std::env::temp_dir().join("viso_program_hlsl");
    std::fs::create_dir_all(&dir).expect("temp dir");
    let file = dir.join(format!("{}.hlsl", p.name));
    std::fs::write(&file, &src).expect("write HLSL");
    for (stage, entry) in [("vert", VERTEX_ENTRY), ("frag", FRAGMENT_ENTRY)] {
        let spv = dir.join(format!("{}.{stage}.spv", p.name));
        let Ok(out) = Command::new("glslangValidator")
            .args([
                "-D",
                "-S",
                stage,
                "-e",
                entry,
                "--target-env",
                "vulkan1.1",
                "-o",
            ])
            .arg(&spv)
            .arg(&file)
            .output()
        else {
            eprintln!("glslangValidator is not installed; HLSL unchecked");
            return;
        };
        assert!(
            out.status.success(),
            "{stage}:\n{}\n--- HLSL ---\n{src}",
            String::from_utf8_lossy(&out.stdout)
        );
        if let Ok(val) = Command::new("spirv-val").arg(&spv).output() {
            assert!(
                val.status.success(),
                "{}",
                String::from_utf8_lossy(&val.stderr)
            );
        }
    }
}

#[test]
fn the_spec_example_compiles() {
    compiles(
        r#"
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
"#,
    );
}

#[test]
fn records_matrices_loops_textures_and_functions_compile() {
    compiles(
        r#"
@shader_value
record Light { dir: Vec3F32; strength: F32; on: U32; }

shader Lit {
    uniform viewport: Vec2F32;
    uniform basis: Mat3F32;
    uniform light: Light;
    uniform mode: I32;
    uniform count: I32;
    instance pos: Vec2F32;
    instance size: Vec2F32;
    instance xform: Mat2F32;
    instance tint: ColorLinear;
    instance layer: U32;
    texture atlas: Texture2D<Vec4F32>;
    texture mask: Texture2D<F32>;
    sampler smp: Sampler;
    sampler nearest: Sampler;
    varying uv: Vec2F32;
    varying slot: U32;

    fn shade(n: Vec3F32, l: Light) -> F32 {
        if l.on == 0u32 { return 0.0; }
        return max(dot(normalize(n), normalize(l.dir)), 0.0) * l.strength;
    }

    fn wave(x: F32) -> F32 {
        return sin(x) * cos(x) + tan(x * 0.1) + asin(0.5) + acos(0.5) + atan(x)
            + sinh(0.1) + cosh(0.1) + tanh(x) + radians(degrees(x));
    }

    vertex(vertex_id: U32, instance_id: U32) -> VertexOutput {
        let unit = quad_vertex(vertex_id);
        let local = xform * (unit * size);
        uv = unit;
        slot = layer + instance_id;
        return VertexOutput { clip_position: to_clip(pos + local, viewport) };
    }

    fragment(frag_coord: Vec4F32) -> Vec4F32 {
        let texel = sample(atlas, smp, uv);
        let coverage = sample_level(mask, nearest, uv, 0.0);
        let n = basis * Vec3F32(uv * 2.0 - 1.0, 1.0);
        let mut sum = 0.0;
        for i in 0..4 { sum += wave(i as F32); }
        @max_iterations(8)
        for j in 0..=count {
            if j == 2 { continue; }
            if j > 5 { break; }
            sum += 0.125;
        }
        let k = match mode { 0 | 1 => 0.25, 2 => 0.5, _ => 1.0 };
        if coverage < 0.01 { discard(); }
        let edge = fwidth(uv.x) + abs(dpdx(uv.y)) + abs(dpdy(uv.y));
        let g = clamp(shade(n, light) * k + sum * 0.01 + edge, 0.0, 1.0);
        let w = step(0.5, uv) + mix(uv, Vec2F32(1.0), 0.5) + smoothstep(0.0, 1.0, uv);
        let misc = floor(g) + ceil(g) + round(g) + trunc(g) + fract(g) + sqrt(g)
            + inverse_sqrt(g + 1.0) + exp(g) + log(g + 1.0) + pow(g, 2.0) + sign(g)
            + length(w) + distance(w, uv) + (g % 0.5) + min(g, 0.5);
        let t = transpose(basis) * cross(n, Vec3F32(0.0, 0.0, 1.0));
        let s = (slot % 4u32) as F32;
        let c = tint.to_vec4() * texel;
        return Vec4F32(c.rgb * g + t * 0.0 + misc * 0.0 + s * 0.0, c.a * coverage)
            + frag_coord.xyzw * 0.0;
    }
}
"#,
    );
}

#[test]
fn a_vertex_entry_samples_a_texture() {
    compiles(
        r#"
shader Displace {
    uniform viewport: Vec2F32;
    instance pos: Vec2F32;
    texture height: Texture2D<F32>;
    sampler smp: Sampler;
    varying lift: F32;

    vertex(vertex_id: U32) -> VertexOutput {
        let unit = quad_vertex(vertex_id);
        let h = sample_level(height, smp, unit, 0.0);
        lift = h;
        return VertexOutput { clip_position: to_clip(pos + unit * h, viewport) };
    }

    fragment() -> Vec4F32 {
        return Vec4F32(lift, lift, lift, 1.0);
    }
}
"#,
    );
}

#[test]
fn a_compile_error_reports_the_backend_log() {
    let backend = MetalBackend::new();
    let desc = ProgramDesc {
        label: "broken",
        msl: "vertex float4 viso_vertex() { return undefined_thing; }",
        vertex_entry: VERTEX_ENTRY,
        fragment_entry: FRAGMENT_ENTRY,
        color_format: TextureFormat::Bgra8Unorm,
        blend: BlendMode::Replace,
        vertex_textures: false,
    };
    let log = backend
        .compiler()
        .compile(&desc)
        .err()
        .expect("a compile error");
    assert!(log.contains("undefined_thing"), "{log}");
    assert_eq!(backend.library_compiles(), 0);
}
