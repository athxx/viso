//! User programs through the renderer on the headless raster: a box painted by
//! its program, adjacent boxes of one program merged into one draw, a
//! parameter change uploaded as one range, a box inside a translucent layer
//! drawn at its place and never folded, a replaced or released program, and a
//! brush reading the clock redrawn as it moves with nothing uploaded.

use std::sync::Arc;

use viso_gpu::{GpuBackend, HeadlessRaster, RawWindowHandle, ShaderLang, SurfaceId};
use viso_render::{
    BatchPipeline, Border, LayerClip, Primitive, ProgramSlot, Quad, Rect, Renderer, Rgba,
    ShaderDraw,
};
use viso_shader::program::{
    BinaryOp, Binding, Block, BrushLayout, Builtin, Expr, ExprKind, Function, Intrinsic, Local,
    ParamValue, Program, Span, Stage, Stmt, Ty, code,
};

const W: u32 = 64;
const H: u32 = 64;
const WHITE: [f32; 4] = [1.0, 1.0, 1.0, 1.0];

fn binding(name: &str, ty: Ty) -> Binding {
    Binding {
        name: name.into(),
        ty,
        span: Span::default(),
    }
}

/// A brush filling its box with its `color` parameter, or with `constant`
/// when given.
fn fill(name: &str, constant: Option<[f32; 4]>) -> Program {
    let e = Expr::new;
    let unit = e(
        Ty::VEC2,
        ExprKind::Intrinsic(Intrinsic::QuadVertex, vec![e(Ty::U32, ExprKind::Local(0))]),
    );
    let position = e(
        Ty::VEC2,
        ExprKind::Binary(
            BinaryOp::Add,
            Box::new(e(Ty::VEC2, ExprKind::Instance(0))),
            Box::new(e(
                Ty::VEC2,
                ExprKind::Binary(
                    BinaryOp::Mul,
                    Box::new(unit),
                    Box::new(e(Ty::VEC2, ExprKind::Instance(1))),
                ),
            )),
        ),
    );
    let clip = e(
        Ty::VEC4,
        ExprKind::Intrinsic(
            Intrinsic::ToClip,
            vec![position, e(Ty::VEC2, ExprKind::Uniform(0))],
        ),
    );
    let color = match constant {
        Some(c) => e(
            Ty::VEC4,
            ExprKind::Construct(c.iter().map(|v| e(Ty::F32, ExprKind::F32(*v))).collect()),
        ),
        None => e(
            Ty::VEC4,
            ExprKind::Intrinsic(Intrinsic::ToVec4, vec![e(Ty::Color, ExprKind::Instance(2))]),
        ),
    };
    let program = Program {
        name: name.into(),
        uniforms: vec![binding("viewport_size", Ty::VEC2)],
        instance: vec![
            binding("rect_pos", Ty::VEC2),
            binding("rect_size", Ty::VEC2),
            binding("color", Ty::Color),
        ],
        vertex: Some(Function {
            name: "vertex".into(),
            stage: Some(Stage::Vertex),
            params: 1,
            builtins: vec![Builtin::VertexId],
            ret: Ty::VertexOutput,
            locals: vec![Local {
                name: "vertex_id".into(),
                ty: Ty::U32,
                mutable: false,
            }],
            body: Block(vec![Stmt::Return(Some(e(
                Ty::VertexOutput,
                ExprKind::Construct(vec![clip]),
            )))]),
            span: Span::default(),
        }),
        fragment: Some(Function {
            name: "fragment".into(),
            stage: Some(Stage::Fragment),
            params: 0,
            builtins: Vec::new(),
            ret: Ty::VEC4,
            locals: Vec::new(),
            body: Block(vec![Stmt::Return(Some(color))]),
            span: Span::default(),
        }),
        ..Program::default()
    };
    program.validate().expect("a valid program");
    program
}

/// A brush filling its box red as far as the clock, `time` seconds, reads.
fn clock(name: &str) -> Program {
    let e = Expr::new;
    let mut program = fill(name, None);
    program.uniforms.push(binding("time", Ty::F32));
    let red = e(
        Ty::VEC4,
        ExprKind::Construct(vec![
            e(Ty::F32, ExprKind::Uniform(1)),
            e(Ty::F32, ExprKind::F32(0.0)),
            e(Ty::F32, ExprKind::F32(0.0)),
            e(Ty::F32, ExprKind::F32(1.0)),
        ]),
    );
    program.fragment.as_mut().unwrap().body = Block(vec![Stmt::Return(Some(red))]);
    program.validate().expect("a valid program");
    program
}

struct Frame {
    gpu: HeadlessRaster,
    surface: SurfaceId,
    renderer: Renderer,
}

impl Frame {
    fn new() -> Frame {
        let mut gpu = HeadlessRaster::new();
        let surface = gpu.create_surface(RawWindowHandle::Headless, W, H);
        let format = gpu.surface_format(surface);
        let mut renderer = Renderer::new(&mut gpu, format);
        renderer.set_surface_size([W as f32, H as f32]);
        Frame {
            gpu,
            surface,
            renderer,
        }
    }

    /// Installs `program` in `slot`; its layout.
    fn install(&mut self, slot: u32, program: &Program) -> BrushLayout {
        let interface = program.interface();
        let layout = BrushLayout::new(program, &interface).expect("a brush");
        let cpu = code(program, &interface, ShaderLang::None).expect("CPU code");
        self.renderer
            .install_program(
                &mut self.gpu,
                ProgramSlot(slot),
                &program.name,
                &cpu,
                layout.clone(),
            )
            .expect("installs");
        layout
    }

    /// Uploads `prims`; the frame's stats are read before submitting.
    fn draw(&mut self, prims: &[Primitive]) -> viso_render::FrameStats {
        self.renderer.upload(&mut self.gpu, prims);
        let stats = self.renderer.frame_stats();
        self.renderer
            .submit(&mut self.gpu, self.surface, WHITE, [W as f32, H as f32]);
        stats
    }

    /// The BGRA8 pixel at `(x, y)`.
    fn pixel(&self, x: u32, y: u32) -> [u8; 4] {
        let px = self.gpu.read_pixels_bgra8(self.surface);
        let i = ((y * W + x) * 4) as usize;
        [px[i], px[i + 1], px[i + 2], px[i + 3]]
    }
}

fn rect(x: f32, y: f32, w: f32, h: f32) -> Rect {
    Rect { x, y, w, h }
}

/// A box of `slot` filled with `color` (premultiplied, linear).
fn shaded(layout: &BrushLayout, slot: u32, r: Rect, color: [f32; 4]) -> Primitive {
    let mut params = Vec::new();
    layout
        .encode(&[ParamValue::ColorLinear(color)], &mut params)
        .expect("encodes");
    Primitive::Shader(ShaderDraw {
        rect: r,
        program: ProgramSlot(slot),
        params: params.into(),
    })
}

fn quad(r: Rect) -> Primitive {
    Primitive::Quad(Quad {
        rect: r,
        color: Rgba {
            r: 0.0,
            g: 0.0,
            b: 0.0,
            a: 1.0,
        },
        radius: 0.0,
        border: Border::NONE,
    })
}

const RED: [f32; 4] = [1.0, 0.0, 0.0, 1.0];
const GREEN: [f32; 4] = [0.0, 1.0, 0.0, 1.0];
const BLUE: [f32; 4] = [0.0, 0.0, 1.0, 1.0];

/// BGRA8 of an opaque premultiplied color.
fn bgra(c: [f32; 4]) -> [u8; 4] {
    let q = |v: f32| (v * 255.0).round() as u8;
    [q(c[2]), q(c[1]), q(c[0]), q(c[3])]
}

#[test]
fn a_program_paints_its_boxes_and_adjacent_boxes_of_one_program_merge() {
    let mut f = Frame::new();
    let layout = f.install(0, &fill("Fill", None));
    let other = f.install(3, &fill("Other", None));
    let prims = [
        shaded(&layout, 0, rect(2.0, 2.0, 10.0, 10.0), RED),
        shaded(&layout, 0, rect(14.0, 2.0, 10.0, 10.0), GREEN),
        quad(rect(26.0, 2.0, 10.0, 10.0)),
        shaded(&layout, 0, rect(38.0, 2.0, 10.0, 10.0), BLUE),
        shaded(&other, 3, rect(50.0, 2.0, 10.0, 10.0), RED),
    ];
    f.renderer.upload(&mut f.gpu, &prims);
    let batches = f.renderer.inspect_batches();
    let shape: Vec<(BatchPipeline, u32)> = batches
        .batches
        .iter()
        .map(|b| (b.pipeline, b.range.1))
        .collect();
    assert_eq!(
        shape,
        [
            (BatchPipeline::Program, 2),
            (BatchPipeline::Quad, 1),
            (BatchPipeline::Program, 1),
            (BatchPipeline::Program, 1),
        ],
        "the two first boxes are one draw; another program is another"
    );
    assert_eq!(
        batches.batches[3].pipeline_id,
        f.renderer.program_pipeline(ProgramSlot(3)).unwrap()
    );
    // The primitive view places each box where the draw reads it.
    let primitives = f.renderer.inspect_primitives();
    let stride = layout.stride;
    let ranges: Vec<(u32, u32)> = primitives.primitives.iter().map(|p| p.range).collect();
    assert_eq!(
        ranges,
        [(0, 1), (stride, 1), (0, 1), (2 * stride, 1), (0, 1)]
    );
    let stats = f.renderer.frame_stats();
    assert_eq!(stats.draw_calls, 4);
    assert_eq!(stats.shader_instances, 4);
    assert_eq!(stats.programs, 2);
    assert_eq!(
        stats.pipeline_switches, 4,
        "two programs side by side switch"
    );
    f.renderer
        .submit(&mut f.gpu, f.surface, WHITE, [W as f32, H as f32]);
    assert_eq!(f.pixel(6, 6), bgra(RED));
    assert_eq!(f.pixel(18, 6), bgra(GREEN));
    assert_eq!(f.pixel(42, 6), bgra(BLUE));
    assert_eq!(f.pixel(54, 6), bgra(RED));
    assert_eq!(f.pixel(6, 20), bgra(WHITE), "nothing outside the boxes");
    assert_eq!(
        f.pixel(12, 6),
        bgra(WHITE),
        "the box's right edge is its own"
    );
}

#[test]
fn an_unchanged_frame_uploads_nothing_and_a_parameter_change_one_range() {
    let mut f = Frame::new();
    let layout = f.install(0, &fill("Fill", None));
    let a = shaded(&layout, 0, rect(2.0, 2.0, 10.0, 10.0), RED);
    let b = shaded(&layout, 0, rect(20.0, 2.0, 10.0, 10.0), GREEN);
    let first = f.draw(&[a.clone(), b.clone()]);
    assert!(first.gpu_upload_bytes > 0);
    let steady = f.draw(&[a.clone(), b]);
    assert_eq!(
        (
            steady.gpu_upload_bytes,
            steady.uploaded_ranges,
            steady.dirty_primitives
        ),
        (0, 0, 0)
    );
    let recolored = shaded(&layout, 0, rect(20.0, 2.0, 10.0, 10.0), BLUE);
    let changed = f.draw(&[a, recolored]);
    assert_eq!(changed.dirty_primitives, 1);
    assert_eq!(changed.uploaded_ranges, 1);
    assert_eq!(
        changed.gpu_upload_bytes, 8,
        "green to blue moves two lanes of the color, the only words uploaded"
    );
    assert_eq!(f.pixel(24, 6), bgra(BLUE));
}

#[test]
fn inside_a_translucent_layer_a_box_draws_at_its_place_and_is_never_folded() {
    let mut f = Frame::new();
    let layout = f.install(0, &fill("Fill", None));
    let stats = f.draw(&[
        Primitive::Layer(LayerClip {
            clip: rect(16.0, 16.0, 32.0, 32.0),
            opacity: 0.5,
            blur_sigma: 0.0,
            backdrop_sigma: 0.0,
        }),
        shaded(&layout, 0, rect(24.0, 24.0, 8.0, 8.0), RED),
        Primitive::LayerEnd,
    ]);
    assert_eq!(
        stats.opacity_folds, 0,
        "a program's output is opaque to folding"
    );
    assert_eq!(stats.offscreen_passes, 1);
    let [b, g, r, a] = f.pixel(28, 28);
    assert!(r == 255 && (120..=135).contains(&g) && (120..=135).contains(&b) && a == 255);
    assert_eq!(
        f.pixel(22, 28),
        bgra(WHITE),
        "left of the box, inside the layer"
    );
    assert_eq!(f.pixel(33, 28), bgra(WHITE), "right of the box");
    assert_eq!(f.pixel(28, 22), bgra(WHITE), "above the box");
}

#[test]
fn a_replaced_program_draws_at_once_and_a_released_one_draws_nothing() {
    let mut f = Frame::new();
    let layout = f.install(0, &fill("Fill", None));
    let first_pipeline = f.renderer.program_pipeline(ProgramSlot(0)).unwrap();
    let r#box = shaded(&layout, 0, rect(8.0, 8.0, 16.0, 16.0), RED);
    f.draw(std::slice::from_ref(&r#box));
    assert_eq!(f.pixel(12, 12), bgra(RED));

    f.install(0, &fill("Blue", Some(BLUE)));
    assert_ne!(
        f.renderer.program_pipeline(ProgramSlot(0)),
        Some(first_pipeline)
    );
    let stats = f.draw(std::slice::from_ref(&r#box));
    assert_eq!(stats.programs, 1);
    assert_eq!(f.pixel(12, 12), bgra(BLUE));

    assert!(f.renderer.release_program(&mut f.gpu, ProgramSlot(0)));
    assert!(!f.renderer.release_program(&mut f.gpu, ProgramSlot(0)));
    let stats = f.draw(std::slice::from_ref(&r#box));
    assert_eq!(
        (
            stats.programs,
            stats.program_draws_dropped,
            stats.draw_calls
        ),
        (0, 1, 0)
    );
    assert_eq!(f.pixel(12, 12), bgra(WHITE));

    // Parameters of another program's stride paint nothing either.
    f.install(0, &fill("Fill", None));
    let foreign = Primitive::Shader(ShaderDraw {
        rect: rect(8.0, 8.0, 16.0, 16.0),
        program: ProgramSlot(0),
        params: Arc::from(vec![0u8; 8]),
    });
    assert_eq!(f.draw(&[foreign]).program_draws_dropped, 1);
}

#[test]
fn a_slot_past_the_key_or_a_wide_uniform_block_is_refused() {
    let mut f = Frame::new();
    let program = fill("Fill", None);
    let interface = program.interface();
    let layout = BrushLayout::new(&program, &interface).unwrap();
    let cpu = code(&program, &interface, ShaderLang::None).unwrap();
    let past = viso_render::batch::BatchKey::PROGRAM_SLOTS;
    let err = f
        .renderer
        .install_program(&mut f.gpu, ProgramSlot(past), "Fill", &cpu, layout.clone())
        .expect_err("past the key");
    assert_eq!(err, viso_render::ProgramError::Slot(ProgramSlot(past)));
    let wide = BrushLayout {
        uniform_size: 32,
        ..layout
    };
    assert!(matches!(
        f.renderer
            .install_program(&mut f.gpu, ProgramSlot(0), "Fill", &cpu, wide),
        Err(viso_render::ProgramError::Uniforms { size: 32 })
    ));
    assert_eq!(f.renderer.frame_stats().programs, 0);
}

/// A scene of program draws — merged, between two programs, clipped and
/// inside a translucent layer — rendered into a fresh BGRA8 target on `gpu`
/// from `lang` code.
fn program_scene<B: GpuBackend>(gpu: &mut B, lang: ShaderLang) -> viso_gpu::TextureId {
    let mut renderer = Renderer::new(gpu, viso_gpu::TextureFormat::Bgra8Unorm);
    renderer.set_surface_size([W as f32, H as f32]);
    let mut layouts = Vec::new();
    for (slot, program) in [(0, fill("Fill", None)), (1, fill("Blue", Some(BLUE)))] {
        let interface = program.interface();
        let layout = BrushLayout::new(&program, &interface).unwrap();
        let program_code = code(&program, &interface, lang).unwrap();
        renderer
            .install_program(
                gpu,
                ProgramSlot(slot),
                &program.name,
                &program_code,
                layout.clone(),
            )
            .unwrap_or_else(|e| panic!("{e}"));
        layouts.push(layout);
    }
    let fill_layout = &layouts[0];
    let prims = [
        shaded(fill_layout, 0, rect(2.0, 2.0, 20.0, 12.0), RED),
        shaded(
            fill_layout,
            0,
            rect(10.0, 8.0, 20.0, 12.0),
            [0.0, 0.5, 0.0, 0.5],
        ),
        shaded(&layouts[1], 1, rect(34.0, 2.0, 12.0, 12.0), BLUE),
        Primitive::Layer(LayerClip {
            clip: rect(4.0, 30.0, 12.0, 12.0),
            opacity: 1.0,
            blur_sigma: 0.0,
            backdrop_sigma: 0.0,
        }),
        shaded(fill_layout, 0, rect(0.0, 26.0, 24.0, 24.0), GREEN),
        Primitive::LayerEnd,
        Primitive::Layer(LayerClip {
            clip: rect(30.0, 30.0, 30.0, 30.0),
            opacity: 0.5,
            blur_sigma: 0.0,
            backdrop_sigma: 0.0,
        }),
        quad(rect(30.0, 30.0, 10.0, 10.0)),
        shaded(fill_layout, 0, rect(36.0, 36.0, 16.0, 16.0), RED),
        Primitive::LayerEnd,
    ];
    renderer.upload(gpu, &prims);
    let target = gpu.create_texture(&viso_gpu::TextureDesc {
        width: W,
        height: H,
        format: viso_gpu::TextureFormat::Bgra8Unorm,
        render_target: true,
        label: "program-scene",
    });
    renderer.render_to_texture(gpu, target, WHITE, [W as f32, H as f32]);
    target
}

#[test]
fn the_program_scene_draws_on_the_headless_raster() {
    let mut gpu = HeadlessRaster::new();
    let target = program_scene(&mut gpu, ShaderLang::None);
    let px = gpu.read_texture(target);
    let at = |x: u32, y: u32| {
        let i = ((y * W + x) * 4) as usize;
        [px[i], px[i + 1], px[i + 2], px[i + 3]]
    };
    assert_eq!(at(4, 4), bgra(RED));
    assert_eq!(at(40, 6), bgra(BLUE));
    // The clipped box covers only its layer's clip.
    assert_eq!(at(8, 34), bgra(GREEN));
    assert_eq!(at(2, 34), bgra(WHITE));
    assert_eq!(at(8, 28), bgra(WHITE));
    // Half green over red, half over white.
    assert_eq!(at(16, 10)[1], 128);
}

#[cfg(target_vendor = "apple")]
#[test]
fn the_program_scene_matches_the_headless_raster_on_metal() {
    let mut headless = HeadlessRaster::new();
    let target = program_scene(&mut headless, ShaderLang::None);
    let reference = headless.read_texture(target);
    let mut metal = viso_gpu::MetalBackend::new();
    let target = program_scene(&mut metal, ShaderLang::Msl);
    let pixels = metal.read_texture(target);
    let worst = pixels
        .iter()
        .zip(&reference)
        .map(|(a, b)| a.abs_diff(*b))
        .max()
        .unwrap();
    let over = pixels
        .as_chunks::<4>()
        .0
        .iter()
        .zip(reference.as_chunks::<4>().0)
        .filter(|(a, b)| a.iter().zip(*b).any(|(a, b)| a.abs_diff(*b) > 2))
        .count();
    assert_eq!(over, 0, "worst channel difference {worst}");
}

#[test]
fn a_brush_program_installs_from_its_code_in_the_backend_language() {
    let mut f = Frame::new();
    let program = fill("Fill", None);
    let interface = program.interface();
    let layout = BrushLayout::new(&program, &interface).unwrap();
    let msl = code(&program, &interface, ShaderLang::Msl).unwrap();
    let mut brush = viso_render::BrushProgram {
        name: "Fill".into(),
        layout: layout.clone(),
        code: vec![msl],
    };
    assert_eq!(
        f.renderer
            .install_brush(&mut f.gpu, ProgramSlot(0), &brush)
            .unwrap_err(),
        viso_render::ProgramError::NoCode(ShaderLang::None)
    );
    brush
        .code
        .push(code(&program, &interface, ShaderLang::None).unwrap());
    f.renderer
        .install_brush(&mut f.gpu, ProgramSlot(0), &brush)
        .unwrap();
    f.draw(&[shaded(&layout, 0, rect(8.0, 8.0, 8.0, 8.0), GREEN)]);
    assert_eq!(f.pixel(10, 10), bgra(GREEN));
}

#[test]
fn a_brush_reading_the_clock_redraws_as_it_moves_and_uploads_nothing() {
    let mut f = Frame::new();
    let clocked = f.install(0, &clock("Clock"));
    assert!(clocked.reads_time());
    let still = f.install(1, &fill("Fill", None));
    let a = shaded(&clocked, 0, rect(2.0, 2.0, 10.0, 10.0), RED);
    let b = shaded(&still, 1, rect(20.0, 2.0, 10.0, 10.0), GREEN);
    let red = |f: &Frame| i32::from(f.pixel(6, 6)[2]);

    f.renderer.set_time(0.25);
    let first = f.draw(&[a.clone(), b.clone()]);
    assert_eq!(first.clock_instances, 1);
    assert!(f.renderer.reads_time());
    assert!((red(&f) - 64).abs() <= 1, "{}", red(&f));

    f.renderer.set_time(0.75);
    let moved = f.draw(&[a.clone(), b.clone()]);
    assert_eq!(
        (
            moved.gpu_upload_bytes,
            moved.uploaded_ranges,
            moved.dirty_primitives
        ),
        (0, 0, 0),
        "the clock rides the draw's uniforms"
    );
    assert!((red(&f) - 191).abs() <= 1, "{}", red(&f));
    assert_eq!(f.pixel(24, 6), bgra(GREEN));

    // Off the target, clipped away, or gone, the box is not drawn and no
    // longer keeps frames coming.
    let away = shaded(&clocked, 0, rect(200.0, 200.0, 10.0, 10.0), RED);
    let stats = f.draw(&[away, b.clone()]);
    assert_eq!((stats.clock_instances, stats.culled_primitives), (0, 1));
    assert_eq!(stats.draw_calls, 1);
    assert!(!f.renderer.reads_time());
    let clipped = [
        Primitive::Layer(LayerClip {
            clip: rect(40.0, 40.0, 8.0, 8.0),
            opacity: 1.0,
            blur_sigma: 0.0,
            backdrop_sigma: 0.0,
        }),
        a.clone(),
        Primitive::LayerEnd,
    ];
    assert_eq!(f.draw(&clipped).clock_instances, 0);
    assert!(!f.renderer.reads_time());
    f.draw(&[a, b.clone()]);
    assert!(f.renderer.reads_time());
    f.draw(&[b]);
    assert!(!f.renderer.reads_time());
}
