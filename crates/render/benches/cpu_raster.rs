//! The CPU raster as an embedded panel's renderer: an 800×480 UI
//! drawn whole, and redrawn where 5% of it changed, on an XRGB8888 panel
//! taking linear and sRGB-encoded bytes and on an RGB565 one, on one thread
//! and on four. A user shader's cards and backdrop over the whole panel,
//! drawn by the interpreter and by the Rust built from the program.
//!
//! Before timing, it prints what each configuration holds — the pixels of
//! the framebuffer and every target, and the workers' scratch — and the heap
//! allocations of a warmed damage frame.
//!
//! Run release (`cargo bench -p viso-render --bench cpu_raster`).

use std::alloc::{GlobalAlloc, Layout, System};
use std::hint::black_box;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use criterion::{Criterion, criterion_group, criterion_main};
use viso_dsl::frontend::{Origin, compile_file};
use viso_gpu::{
    BindGroupDesc, Binding, BlendMode, BufferDesc, BufferUsage, ColorSpace, CpuRaster, DrawCommand,
    DrawList, Geometry, GpuBackend, InlineUniforms, LoadOp, ProgramCode, ProgramCompiler,
    ProgramDesc, RawWindowHandle, RenderPass, RenderTarget, SurfaceId, TextureDesc, TextureFormat,
    TextureId,
};
use viso_render::{
    Border, ExtendMode, Gradient, GradientKind, GradientStop, InterpolationSpace, LayerClip, Point,
    Primitive, Quad, Rect, Renderer, Rgba,
};
use viso_shader::program::interp::NativeKernel;
use viso_shader::program::{CpuShader, FRAGMENT_ENTRY, Program, VERTEX_ENTRY, Value};

struct CountingAlloc;

static ALLOCS: AtomicUsize = AtomicUsize::new(0);
static ARMED: AtomicBool = AtomicBool::new(false);

// SAFETY: forwards every call to the system allocator unchanged; the only added
// behavior is a relaxed counter increment on allocation while armed.
unsafe impl GlobalAlloc for CountingAlloc {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        if ARMED.load(Ordering::Relaxed) {
            ALLOCS.fetch_add(1, Ordering::Relaxed);
        }
        // SAFETY: `layout` is the caller's, valid for `alloc`; the system allocator gets it unchanged.
        unsafe { System.alloc(layout) }
    }
    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        // SAFETY: `ptr` was allocated by `System` with `layout`, through this allocator.
        unsafe { System.dealloc(ptr, layout) }
    }
    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        if ARMED.load(Ordering::Relaxed) {
            ALLOCS.fetch_add(1, Ordering::Relaxed);
        }
        // SAFETY: `ptr` was allocated by `System` with `layout`, and the caller upholds `realloc`'s contract.
        unsafe { System.realloc(ptr, layout, new_size) }
    }
}

#[global_allocator]
static ALLOCATOR: CountingAlloc = CountingAlloc;

const W: u32 = 800;
const H: u32 = 480;
const CLEAR: [f32; 4] = [0.07, 0.08, 0.1, 1.0];

fn rect(x: f32, y: f32, w: f32, h: f32) -> Rect {
    Rect { x, y, w, h }
}

fn quad(r: Rect, color: Rgba, radius: f32) -> Primitive {
    Primitive::Quad(Quad {
        rect: r,
        color,
        radius,
        border: Border::NONE,
    })
}

/// A dashboard: a header gradient, a grid of rounded cards with rows of
/// "text" bars, a translucent overlay panel, and a `highlight` tile of 5%
/// of the screen whose color is the frame's one change.
fn dashboard(highlight: f32) -> Vec<Primitive> {
    let mut scene = vec![Primitive::Gradient(Gradient {
        rect: rect(0.0, 0.0, W as f32, 56.0),
        kind: GradientKind::Linear,
        extend: ExtendMode::Clamp,
        p0: Point { x: 0.0, y: 0.0 },
        p1: Point {
            x: W as f32,
            y: 56.0,
        },
        stops: vec![
            GradientStop {
                offset: 0.0,
                color: Rgba::new(0.1, 0.3, 0.7, 1.0),
            },
            GradientStop {
                offset: 1.0,
                color: Rgba::new(0.5, 0.2, 0.6, 1.0),
            },
        ],
        interp: InterpolationSpace::LinearRgb,
        mask: None,
    })];
    for row in 0..3 {
        for col in 0..4 {
            let x = 16.0 + col as f32 * 196.0;
            let y = 72.0 + row as f32 * 136.0;
            scene.push(quad(
                rect(x, y, 180.0, 120.0),
                Rgba::new(0.16, 0.17, 0.2, 1.0),
                10.0,
            ));
            for line in 0..5 {
                let w = 150.0 - (line * 17 % 60) as f32;
                scene.push(quad(
                    rect(x + 14.0, y + 14.0 + line as f32 * 20.0, w, 10.0),
                    Rgba::new(0.75, 0.77, 0.8, 1.0),
                    3.0,
                ));
            }
        }
    }
    // 196×98 is 5% of 800×480.
    scene.push(quad(
        rect(16.0, 360.0, 196.0, 98.0),
        Rgba::new(highlight, 0.6, 0.3, 1.0),
        12.0,
    ));
    scene.push(Primitive::Layer(LayerClip {
        clip: rect(480.0, 300.0, 300.0, 160.0),
        opacity: 0.85,
        blur_sigma: 0.0,
        backdrop_sigma: 0.0,
    }));
    scene.push(quad(
        rect(480.0, 300.0, 300.0, 160.0),
        Rgba::new(0.95, 0.95, 0.97, 1.0),
        14.0,
    ));
    scene.push(quad(
        rect(500.0, 320.0, 120.0, 120.0),
        Rgba::new(0.2, 0.6, 0.9, 1.0),
        60.0,
    ));
    scene.push(Primitive::LayerEnd);
    scene
}

struct Panel {
    gpu: CpuRaster,
    surface: SurfaceId,
    r: Renderer,
    frame: u32,
}

impl Panel {
    fn new(format: TextureFormat, workers: usize) -> Panel {
        let mut gpu = CpuRaster::with_workers(workers);
        let surface = gpu.create_surface(RawWindowHandle::Headless, W, H);
        gpu.set_surface_color_target(surface, format, ColorSpace::Srgb);
        let format = gpu.surface_format(surface);
        let mut r = Renderer::new(&mut gpu, format);
        r.set_surface_size([W as f32, H as f32], 1.0);
        Panel {
            gpu,
            surface,
            r,
            frame: 0,
        }
    }

    fn present(&mut self, scene: &[Primitive]) {
        self.r.upload(&mut self.gpu, scene);
        self.r
            .submit(&mut self.gpu, self.surface, CLEAR, [W as f32, H as f32]);
    }

    /// A frame where only the highlight tile changed.
    fn damage_frame(&mut self, scenes: &[Vec<Primitive>; 2]) {
        self.frame += 1;
        self.present(&scenes[(self.frame % 2) as usize]);
    }

    /// The whole frame, redrawn: a fresh surface every time.
    fn full_frame(&mut self, scene: &[Primitive]) {
        self.gpu.resize_surface(self.surface, W, H);
        self.present(scene);
    }
}

fn report(name: &str, format: TextureFormat, workers: usize, scenes: &[Vec<Primitive>; 2]) {
    let mut p = Panel::new(format, workers);
    for _ in 0..4 {
        p.damage_frame(scenes);
    }
    ALLOCS.store(0, Ordering::Relaxed);
    ARMED.store(true, Ordering::Relaxed);
    p.damage_frame(scenes);
    ARMED.store(false, Ordering::Relaxed);
    eprintln!(
        "{name}: pixel_bytes {} scratch_bytes {} deferred_layers {} damage-frame allocations {}",
        p.gpu.pixel_bytes(),
        p.gpu.scratch_bytes(),
        p.gpu.deferred_layers(),
        ALLOCS.load(Ordering::Relaxed),
    );
}

/// A user shader and the Rust built from it at build time.
macro_rules! user_shader {
    ($source:literal) => {
        (
            $source,
            viso_ui_macros::__shader_kernel!(::viso_shader, $source),
        )
    };
}

/// The rounded rectangle of the shader language's own example.
const ROUNDED: (&str, &NativeKernel) = user_shader!(
    r#"
shader Rounded {
    instance radius: F32;
    instance color: ColorLinear;
    varying local_pos: Vec2F32;

    vertex(vertex_id: U32) -> VertexOutput {
        let unit = quad_vertex(vertex_id);
        local_pos = unit * rect_size;
        return VertexOutput { clip_position: to_clip(rect_pos + local_pos, viewport_size) };
    }

    fragment() -> Vec4F32 {
        let distance = rounded_rect_sdf(local_pos, rect_size, radius);
        let alpha = smoothstep(1.0f32, 0.0f32, distance);
        return color.to_vec4() * alpha;
    }
}
"#
);

/// A card glowing in rings, through a function with a loop.
const RIPPLE: (&str, &NativeKernel) = user_shader!(
    r#"
shader Ripple {
    instance radius: F32;
    instance color: ColorLinear;
    varying local_pos: Vec2F32;

    fn rings(d: F32, t: F32) -> F32 {
        let mut glow = 0.0;
        @max_iterations(4)
        for i in 0..4 {
            let edge = (i as F32) * 12.0 + t;
            glow += smoothstep(edge, edge + 4.0, d) * 0.2;
        }
        return glow;
    }

    vertex(vertex_id: U32) -> VertexOutput {
        let unit = quad_vertex(vertex_id);
        local_pos = unit * rect_size;
        return VertexOutput { clip_position: to_clip(rect_pos + local_pos, viewport_size) };
    }

    fragment() -> Vec4F32 {
        let distance = rounded_rect_sdf(local_pos, rect_size, radius);
        let alpha = smoothstep(1.0f32, 0.0f32, distance);
        let glow = rings(length(local_pos - rect_size * 0.5), time);
        let rgb = color.to_vec4().rgb * (0.6 + glow);
        return Vec4F32(rgb, 1.0) * (color.a * alpha);
    }
}
"#
);

fn user_program(source: &str) -> Program {
    let origin = Origin {
        package: "app".to_owned(),
        module: vec!["main".to_owned()],
        language: None,
        ..Default::default()
    };
    let compiled = compile_file(source, &origin);
    assert!(
        compiled.errors().next().is_none(),
        "the bench shader compiles"
    );
    compiled
        .shaders
        .into_iter()
        .find_map(|shader| shader.program)
        .expect("a program")
}

/// One user shader drawn on the CPU raster: the backdrop and 12 cards, every
/// pixel of the panel covered once and the cards' twice.
struct UserDraw {
    gpu: CpuRaster,
    target: TextureId,
    commands: [DrawCommand; 1],
}

impl UserDraw {
    fn new(program: &Program, native: Option<&'static NativeKernel>) -> UserDraw {
        let interface = program.interface();
        let mut gpu = CpuRaster::new();
        let shader = match native {
            Some(kernel) => CpuShader::with_native(program.clone(), kernel),
            None => CpuShader::new(program.clone()),
        };
        assert_eq!(shader.is_native(), native.is_some());
        let compiled = gpu
            .program_compiler()
            .compile(&ProgramDesc {
                label: &program.name,
                code: &ProgramCode::Cpu(Arc::new(shader)),
                vertex_entry: VERTEX_ENTRY,
                fragment_entry: FRAGMENT_ENTRY,
                color_format: TextureFormat::Bgra8Unorm,
                blend: BlendMode::PremultipliedOver,
                abi: interface.abi(),
            })
            .expect("the CPU program");
        let pipeline = gpu.install_program(compiled);
        let uniforms = interface
            .uniforms
            .encode(
                &program.uniforms,
                &[Value::floats(&[W as f32, H as f32]), Value::f32(3.0)],
            )
            .expect("uniforms encode");
        let mut rows = vec![(0.0, 0.0, W as f32, H as f32, 0.0, [0.1, 0.1, 0.12, 1.0])];
        for row in 0..3 {
            for col in 0..4 {
                let (x, y) = (16.0 + col as f32 * 196.0, 72.0 + row as f32 * 136.0);
                rows.push((x, y, 180.0, 120.0, 12.0, [0.3, 0.5, 0.8, 0.9]));
            }
        }
        let instances: Vec<u8> = rows
            .iter()
            .flat_map(|&(x, y, w, h, r, c)| {
                let row = [
                    Value::floats(&[x, y]),
                    Value::floats(&[w, h]),
                    Value::f32(r),
                    Value::floats(&c),
                ];
                interface
                    .instance
                    .encode(&program.instance, &row)
                    .expect("instance encodes")
            })
            .collect();
        let mut buffer = |bytes: &[u8], usage| {
            let id = gpu.create_buffer(&BufferDesc {
                size: bytes.len().max(16),
                usage,
                label: "user",
            });
            gpu.write_buffer(id, 0, bytes);
            id
        };
        let uniform_buffer = buffer(&uniforms, BufferUsage::UNIFORM);
        let instance_buffer = buffer(&instances, BufferUsage::INSTANCE);
        let bind_group = gpu.create_bind_group(&BindGroupDesc {
            label: "user",
            bindings: vec![Binding::Uniform(uniform_buffer)],
        });
        let target = gpu.create_texture(&TextureDesc {
            width: W,
            height: H,
            format: TextureFormat::Bgra8Unorm,
            render_target: true,
            label: "user-target",
        });
        let commands = [DrawCommand {
            pipeline,
            bind_group: Some(bind_group),
            geometry: Geometry::Generated {
                count: rows.len() as u32,
            },
            instance_buffer,
            instance_offset: 0,
            uniforms: InlineUniforms::new(&[]),
            scissor: None,
        }];
        UserDraw {
            gpu,
            target,
            commands,
        }
    }

    fn draw(&mut self) {
        let passes = [RenderPass {
            target: RenderTarget::Texture(self.target),
            load: LoadOp::Clear(CLEAR),
            depth: None,
            first_command: 0,
            command_count: 1,
        }];
        self.gpu.encode(&DrawList {
            commands: &self.commands,
            passes: &passes,
        });
    }
}

fn bench(c: &mut Criterion) {
    for (name, (source, kernel)) in [("rounded", ROUNDED), ("ripple", RIPPLE)] {
        let program = user_program(source);
        let mut interpreted = UserDraw::new(&program, None);
        let mut native = UserDraw::new(&program, Some(kernel));
        interpreted.draw();
        native.draw();
        assert!(
            interpreted.gpu.read_texture(interpreted.target)
                == native.gpu.read_texture(native.target),
            "{name}: the Rust draws what the interpreter draws"
        );
        c.bench_function(
            &format!("cpu_raster_800x480_user_{name}_interpreted"),
            |b| {
                b.iter(|| interpreted.draw());
            },
        );
        c.bench_function(&format!("cpu_raster_800x480_user_{name}_native"), |b| {
            b.iter(|| native.draw());
        });
    }

    let scenes = [dashboard(0.9), dashboard(0.3)];
    let configs = [
        ("xrgb8888", TextureFormat::Bgra8Unorm, 0),
        ("xrgb8888_srgb", TextureFormat::Bgra8UnormSrgb, 0),
        ("xrgb8888_4t", TextureFormat::Bgra8Unorm, 3),
        ("rgb565", TextureFormat::Rgb565Unorm, 0),
    ];
    for (name, format, workers) in configs {
        report(name, format, workers, &scenes);
        let mut p = Panel::new(format, workers);
        c.bench_function(&format!("cpu_raster_800x480_full_{name}"), |b| {
            b.iter(|| p.full_frame(black_box(&scenes[0])));
        });
        let mut p = Panel::new(format, workers);
        p.present(&scenes[0]);
        c.bench_function(&format!("cpu_raster_800x480_damage_5pct_{name}"), |b| {
            b.iter(|| p.damage_frame(black_box(&scenes)));
        });
    }
}

criterion_group!(benches, bench);
criterion_main!(benches);
