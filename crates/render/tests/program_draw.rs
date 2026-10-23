//! User programs through the renderer on the headless raster: a box painted by
//! its program, adjacent boxes of one program merged into one draw, a
//! parameter change uploaded as one range, a box inside a translucent layer
//! drawn at its place and never folded, a replaced or released program, a
//! brush reading the clock redrawn as it moves with nothing uploaded, a
//! brush sampling its box's image, and pipelines kept by their code.

use std::sync::Arc;

use viso_gpu::{
    AddressMode, CpuRaster, FilterMode, GpuBackend, RawWindowHandle, SamplerDesc, ShaderLang,
    SurfaceId, TextureDesc, TextureFormat, TextureId, srgb,
};
use viso_render::{
    BatchPipeline, Border, LayerClip, Primitive, ProgramSlot, Quad, Rect, Renderer, Rgba,
    ShaderDraw,
};
use viso_shader::program::{
    BinaryOp, Binding, Block, BrushLayout, Builtin, Expr, ExprKind, Function, Intrinsic, Local,
    ParamValue, Program, Span, Stage, Stmt, Texel, Ty, code,
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

/// A brush filling its box with the texel at the middle of its image.
fn imaged(name: &str) -> Program {
    let e = Expr::new;
    let mut program = fill(name, None);
    program
        .textures
        .push(binding("image", Ty::Texture(Texel::Vec4F32)));
    program.samplers.push(binding("pixels", Ty::Sampler));
    let texel = e(
        Ty::VEC4,
        ExprKind::Intrinsic(
            Intrinsic::Sample,
            vec![
                e(Ty::Texture(Texel::Vec4F32), ExprKind::Texture(0)),
                e(Ty::Sampler, ExprKind::Sampler(0)),
                e(
                    Ty::VEC2,
                    ExprKind::Construct(vec![
                        e(Ty::F32, ExprKind::F32(0.5)),
                        e(Ty::F32, ExprKind::F32(0.5)),
                    ]),
                ),
            ],
        ),
    );
    program.fragment.as_mut().unwrap().body = Block(vec![Stmt::Return(Some(texel))]);
    program.validate().expect("a valid program");
    program
}

struct Frame {
    gpu: CpuRaster,
    surface: SurfaceId,
    renderer: Renderer,
}

impl Frame {
    fn new() -> Frame {
        let mut gpu = CpuRaster::new();
        let surface = gpu.create_surface(RawWindowHandle::Headless, W, H);
        let format = gpu.surface_format(surface);
        let mut renderer = Renderer::new(&mut gpu, format);
        renderer.set_surface_size([W as f32, H as f32], 1.0);
        Frame {
            gpu,
            surface,
            renderer,
        }
    }

    /// Installs `program` in `slot`; its layout.
    fn install(&mut self, slot: u32, program: &Program) -> BrushLayout {
        self.install_sampling(slot, program, None)
    }

    /// Installs `program` in `slot`, its sampler's state `sampler` when
    /// given; its layout.
    fn install_sampling(
        &mut self,
        slot: u32,
        program: &Program,
        sampler: Option<SamplerDesc>,
    ) -> BrushLayout {
        let interface = program.interface();
        let mut layout = BrushLayout::new(program, &interface).expect("a brush");
        if let Some(sampler) = sampler {
            layout.samplers = vec![sampler];
        }
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

    /// A 1×1 image of `color`, premultiplied sRGB RGBA8.
    fn image(&mut self, color: [f32; 4]) -> TextureId {
        let texture = self.gpu.create_texture(&TextureDesc {
            width: 1,
            height: 1,
            format: TextureFormat::Rgba8UnormSrgb,
            render_target: false,
            label: "image",
        });
        let e = srgb::encode;
        let texel = [
            e(color[0]),
            e(color[1]),
            e(color[2]),
            (color[3] * 255.0).round() as u8,
        ];
        self.gpu.write_texture(texture, 0, 0, 1, 1, &texel);
        texture
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
    Primitive::Shader(Box::new(ShaderDraw {
        rect: r,
        program: ProgramSlot(slot),
        params: params.into(),
        textures: Default::default(),
    }))
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

/// BGRA8 of an opaque premultiplied color, as the sRGB surface stores it.
fn bgra(c: [f32; 4]) -> [u8; 4] {
    let e = srgb::encode;
    [e(c[2]), e(c[1]), e(c[0]), (c[3] * 255.0).round() as u8]
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
    // Half red over white: half the light in green and blue, sRGB 188.
    assert!(r == 255 && (183..=193).contains(&g) && (183..=193).contains(&b) && a == 255);
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
    let foreign = Primitive::Shader(Box::new(ShaderDraw {
        rect: rect(8.0, 8.0, 16.0, 16.0),
        program: ProgramSlot(0),
        params: Arc::from(vec![0u8; 8]),
        textures: Default::default(),
    }));
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
    // One block past what a draw carries inline.
    let wide = BrushLayout {
        uniform_size: 48,
        ..layout
    };
    assert!(matches!(
        f.renderer
            .install_program(&mut f.gpu, ProgramSlot(0), "Fill", &cpu, wide),
        Err(viso_render::ProgramError::Uniforms { size: 48 })
    ));
    assert_eq!(f.renderer.frame_stats().programs, 0);
}

/// A scene of program draws — merged, between two programs, clipped and
/// inside a translucent layer — rendered into a fresh sRGB BGRA8 target on `gpu`
/// from `lang` code.
fn program_scene<B: GpuBackend>(gpu: &mut B, lang: ShaderLang) -> viso_gpu::TextureId {
    let mut renderer = Renderer::new(gpu, viso_gpu::TextureFormat::Bgra8UnormSrgb);
    renderer.set_surface_size([W as f32, H as f32], 1.0);
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
        format: viso_gpu::TextureFormat::Bgra8UnormSrgb,
        render_target: true,
        label: "program-scene",
    });
    renderer.render_to_texture(gpu, target, WHITE, [W as f32, H as f32]);
    target
}

#[test]
fn the_program_scene_draws_on_the_headless_raster() {
    let mut gpu = CpuRaster::new();
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
    // Half green over red, half over white: half the light, sRGB 188.
    assert_eq!(at(16, 10)[1], 188);
}

#[cfg(target_vendor = "apple")]
#[test]
fn the_program_scene_matches_the_headless_raster_on_metal() {
    let mut headless = CpuRaster::new();
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
fn a_program_of_held_code_compiles_nothing_and_edits_hold_a_bounded_set_of_pipelines() {
    let mut f = Frame::new();
    let brush = |program: &Program| {
        let interface = program.interface();
        viso_render::BrushProgram {
            name: program.name.clone(),
            layout: BrushLayout::new(program, &interface).unwrap(),
            code: vec![code(program, &interface, ShaderLang::None).unwrap()],
        }
    };
    let red = brush(&fill("Fill", None));
    for slot in [ProgramSlot(0), ProgramSlot(1)] {
        f.renderer.install_brush(&mut f.gpu, slot, &red).unwrap();
    }
    assert_eq!(f.renderer.program_compiles(), 1, "one code, one pipeline");
    assert_eq!(
        f.renderer.program_pipeline(ProgramSlot(0)),
        f.renderer.program_pipeline(ProgramSlot(1))
    );

    // A thousand edits of the program in slot 0: each compiles once, the
    // pipelines no slot draws with go past the bound, and coming back to the
    // last one before compiles nothing.
    let edit = |n: u32| brush(&fill("Fill", Some([n as f32 / 1000.0, 0.0, 0.0, 1.0])));
    let mut edits: Vec<_> = (0..1000).map(edit).collect();
    for program in &edits {
        f.renderer
            .install_brush(&mut f.gpu, ProgramSlot(0), program)
            .unwrap();
    }
    assert_eq!(f.renderer.program_compiles(), 1001);
    let r#box = shaded(&red.layout, 0, rect(8.0, 8.0, 16.0, 16.0), RED);
    let stats = f.draw(std::slice::from_ref(&r#box));
    assert_eq!(stats.programs, 2);
    assert_eq!(
        stats.program_pipelines as usize,
        2 + viso_render::IDLE_PIPELINES,
        "the two drawn with and the idle bound"
    );
    let undo = edits.swap_remove(998);
    f.renderer
        .install_brush(&mut f.gpu, ProgramSlot(0), &undo)
        .unwrap();
    assert_eq!(f.renderer.program_compiles(), 1001);
    f.draw(std::slice::from_ref(&r#box));
    assert_eq!(f.pixel(12, 12), bgra([998.0 / 1000.0, 0.0, 0.0, 1.0]));

    // Released, the slots' pipelines idle within the bound too.
    f.renderer.release_program(&mut f.gpu, ProgramSlot(0));
    f.renderer.release_program(&mut f.gpu, ProgramSlot(1));
    let stats = f.draw(&[]);
    assert_eq!(stats.programs, 0);
    assert_eq!(
        stats.program_pipelines as usize,
        viso_render::IDLE_PIPELINES
    );
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
    // A quarter of the light, sRGB 137.
    assert!((red(&f) - 137).abs() <= 1, "{}", red(&f));

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
    // Three quarters of the light, sRGB 225.
    assert!((red(&f) - 225).abs() <= 1, "{}", red(&f));
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

#[test]
fn a_brush_samples_its_boxs_image_and_merges_only_over_the_same_one() {
    let mut f = Frame::new();
    let layout = f.install(0, &imaged("Image"));
    assert_eq!(layout.abi().textures, 1);
    let (red, blue) = (f.image(RED), f.image(BLUE));
    let boxed = |x: f32, image: Option<TextureId>| {
        let Primitive::Shader(mut draw) = shaded(&layout, 0, rect(x, 2.0, 10.0, 10.0), WHITE)
        else {
            unreachable!()
        };
        draw.textures[0] = image;
        Primitive::Shader(draw)
    };
    let scene = [
        boxed(2.0, Some(red)),
        boxed(14.0, Some(red)),
        boxed(26.0, Some(blue)),
    ];
    let first = f.draw(&scene);
    assert_eq!(
        first.draw_calls, 2,
        "the same image merges, another does not"
    );
    assert_eq!(f.pixel(6, 6), bgra(RED));
    assert_eq!(f.pixel(18, 6), bgra(RED));
    assert_eq!(f.pixel(30, 6), bgra(BLUE));
    let bind_groups = f.gpu.bind_group_count();

    // The same images bind the same groups; a swap uploads no instance.
    assert_eq!(f.draw(&scene).gpu_upload_bytes, 0);
    let swapped = [
        boxed(2.0, Some(blue)),
        boxed(14.0, Some(red)),
        boxed(26.0, Some(blue)),
    ];
    let swap = f.draw(&swapped);
    assert_eq!((swap.dirty_primitives, swap.gpu_upload_bytes), (1, 0));
    assert_eq!(swap.draw_calls, 3);
    assert_eq!(f.pixel(6, 6), bgra(BLUE));
    assert_eq!(f.gpu.bind_group_count(), bind_groups, "no new texture set");

    // A box missing its image draws nothing.
    let missing = f.draw(&[boxed(2.0, None)]);
    assert_eq!(missing.program_draws_dropped, 1);
    assert_eq!(missing.draw_calls, 0);

    // A texture going away takes its bind group: drawn again, it binds anew.
    f.renderer.release_texture(&mut f.gpu, red);
    let again = f.image(GREEN);
    f.draw(&[boxed(2.0, Some(again))]);
    assert_eq!(f.gpu.bind_group_count(), bind_groups + 1);
    assert_eq!(f.pixel(6, 6), bgra(GREEN));
}

#[test]
fn a_brush_samples_through_the_state_its_sampler_declares() {
    let mut f = Frame::new();
    let program = imaged("Image");
    let smooth = f.install(0, &program);
    let nearest = f.install_sampling(
        1,
        &program,
        Some(SamplerDesc {
            filter: FilterMode::Nearest,
            address: AddressMode::ClampToEdge,
        }),
    );
    // Red then blue: the middle is their boundary.
    let image = f.gpu.create_texture(&TextureDesc {
        width: 2,
        height: 1,
        format: TextureFormat::Rgba8UnormSrgb,
        render_target: false,
        label: "pair",
    });
    f.gpu
        .write_texture(image, 0, 0, 2, 1, &[255, 0, 0, 255, 0, 0, 255, 255]);
    let boxed = |layout: &BrushLayout, slot: u32, x: f32| {
        let Primitive::Shader(mut draw) = shaded(layout, slot, rect(x, 2.0, 10.0, 10.0), WHITE)
        else {
            unreachable!()
        };
        draw.textures[0] = Some(image);
        Primitive::Shader(draw)
    };
    f.draw(&[boxed(&smooth, 0, 2.0), boxed(&nearest, 1, 20.0)]);
    let [b, g, r, _] = f.pixel(6, 6);
    // Halfway in linear light: sRGB 188 each.
    assert!(
        r.abs_diff(188) <= 1 && b.abs_diff(188) <= 1 && g == 0,
        "bilinear blends the pair: {r} {g} {b}"
    );
    assert_eq!(f.pixel(24, 6), bgra(BLUE), "nearest takes one texel");
}
