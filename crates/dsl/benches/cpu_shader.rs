//! The headless raster's cost for a custom shader: one box covering a
//! 1280×720 target, drawn by a program the CPU runs, a quad of pixels at a
//! time. `ripple` is arithmetic and intrinsics; `lens` samples a texture.
//!
//! Run release (`cargo bench -p viso-dsl --bench cpu_shader`); criterion
//! defaults to a release profile. Debug timing is not a perf result.

use std::hint::black_box;

use criterion::{Criterion, criterion_group, criterion_main};
use viso_dsl::frontend::{Origin, compile_file};
use viso_shader::program::{
    AddressMode, Bindings, BlendMode, CpuTexels, CpuTexture, FilterMode, Program, Raster,
    SamplerDesc, Value,
};

const SOURCE: &str = r#"
import viso::math::Vec2F32;

shader Ripple {
    uniform viewport_size: Vec2F32;
    uniform time: F32;
    instance rect_pos: Vec2F32;
    instance rect_size: Vec2F32;
    instance tint: ColorLinear;
    instance rings: F32;
    instance center: Vec2F32;
    varying local: Vec2F32;

    vertex(vertex_id: U32) -> VertexOutput {
        let unit = quad_vertex(vertex_id);
        local = unit;
        return VertexOutput {
            clip_position: to_clip(rect_pos + unit * rect_size, viewport_size),
        };
    }

    fragment() -> ColorLinear {
        let d = distance(local, center);
        let wave = 0.5 + 0.5 * cos((d * rings - time * 0.5) * 6.2831855);
        let fade = 1.0 - smoothstep(0.3, 0.5, d);
        let k = wave * fade;
        return ColorLinear(tint.r * k, tint.g * k, tint.b * k, 1.0);
    }
}

shader Lens {
    uniform viewport_size: Vec2F32;
    uniform time: F32;
    instance rect_pos: Vec2F32;
    instance rect_size: Vec2F32;
    instance tiles: F32;
    texture image: Texture2D<Vec4F32>;
    @sampler(filter: linear, address: repeat)
    sampler pixels: Sampler;
    varying local: Vec2F32;

    vertex(vertex_id: U32) -> VertexOutput {
        let unit = quad_vertex(vertex_id);
        local = unit;
        return VertexOutput {
            clip_position: to_clip(rect_pos + unit * rect_size, viewport_size),
        };
    }

    fragment() -> Vec4F32 {
        let d = (local - Vec2F32(0.5, 0.5)) * rect_size;
        let bend = 1.0 + 0.08 * sin(length(d) * 0.1 - time * 3.0);
        let uv = (Vec2F32(0.5, 0.5) + d * bend / rect_size) * Vec2F32(tiles * 3.0, tiles);
        return sample(image, pixels, uv);
    }
}
"#;

const WIDTH: u32 = 1280;
const HEIGHT: u32 = 720;

fn program(name: &str) -> Program {
    let origin = Origin {
        package: "app".to_owned(),
        module: vec!["main".to_owned()],
        language: None,
    };
    let compiled = compile_file(SOURCE, &origin);
    assert!(!compiled.has_errors(), "{:?}", compiled.diagnostics);
    compiled
        .shaders
        .into_iter()
        .find(|s| s.name == name)
        .and_then(|s| s.program)
        .expect("a checked program")
}

fn shaders(c: &mut Criterion) {
    let viewport = Value::floats(&[WIDTH as f32, HEIGHT as f32]);
    let full = [
        Value::floats(&[0.0, 0.0]),
        Value::floats(&[WIDTH as f32, HEIGHT as f32]),
    ];
    let uniforms = [viewport, Value::f32(1.25)];

    let ripple = program("Ripple");
    let instances = vec![
        [
            full.to_vec(),
            vec![
                Value::floats(&[0.2, 0.5, 0.9, 1.0]),
                Value::f32(8.0),
                Value::floats(&[0.5, 0.5]),
            ],
        ]
        .concat(),
    ];
    let bindings = Bindings {
        uniforms: &uniforms,
        textures: &[],
        samplers: &[],
    };
    let mut raster = Raster::new(WIDTH, HEIGHT, [0.0; 4]);
    c.bench_function("cpu_shader/ripple_1280x720", |b| {
        b.iter(|| {
            raster.draw(&ripple, &bindings, &instances, BlendMode::PremultipliedOver);
            black_box(&raster.pixels);
        });
    });

    let lens = program("Lens");
    let texels: Vec<[f32; 4]> = (0..64 * 64)
        .map(|i| {
            let (x, y) = ((i % 64) as f32 / 63.0, (i / 64) as f32 / 63.0);
            [x, y, 1.0 - x, 1.0]
        })
        .collect();
    let texture = CpuTexture {
        width: 64,
        height: 64,
        texels: CpuTexels::Float(&texels),
    };
    let sampler = SamplerDesc {
        filter: FilterMode::Linear,
        address: AddressMode::Repeat,
    };
    let instances = vec![[full.to_vec(), vec![Value::f32(2.0)]].concat()];
    let bindings = Bindings {
        uniforms: &uniforms,
        textures: std::slice::from_ref(&texture),
        samplers: std::slice::from_ref(&sampler),
    };
    c.bench_function("cpu_shader/lens_1280x720", |b| {
        b.iter(|| {
            raster.draw(&lens, &bindings, &instances, BlendMode::Replace);
            black_box(&raster.pixels);
        });
    });
}

criterion_group! {
    name = benches;
    config = Criterion::default().sample_size(10);
    targets = shaders
}
criterion_main!(benches);
