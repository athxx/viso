//! A user program on the headless raster: compiled by the headless compiler,
//! installed at a frame boundary, drawn with inline uniforms, an instance
//! offset and a scissor, and retired like any pipeline.

use std::sync::Arc;

use viso_gpu::backend::{
    DrawCommand, DrawList, Geometry, InlineUniforms, RenderPass, RenderTarget,
};
use viso_gpu::{
    BlendMode, BufferDesc, BufferUsage, CpuDraw, CpuProgram, CpuTarget, GpuBackend, HeadlessRaster,
    LoadOp, ProgramAbi, ProgramCode, ProgramCompiler, ProgramDesc, RawWindowHandle, TextureFormat,
};

/// Fills each instance's `[x, y, w, h]` rect with the uniform color.
struct FillRects;

impl CpuProgram for FillRects {
    fn draw(&self, target: &mut CpuTarget<'_>, draw: &CpuDraw<'_>, _: BlendMode) {
        let f = |b: &[u8], i: usize| f32::from_le_bytes(b[i * 4..i * 4 + 4].try_into().unwrap());
        let color = [
            f(draw.uniforms, 0),
            f(draw.uniforms, 1),
            f(draw.uniforms, 2),
            f(draw.uniforms, 3),
        ];
        for inst in draw
            .instances
            .as_chunks::<16>()
            .0
            .iter()
            .take(draw.count as usize)
        {
            let [x, y, w, h] = [0, 1, 2, 3].map(|i| f(inst, i) as u32);
            let [sx0, sy0, sx1, sy1] = target.scissor;
            for py in y.max(sy0)..(y + h).min(sy1) {
                for px in x.max(sx0)..(x + w).min(sx1) {
                    target.pixels[(py * target.width + px) as usize] = color;
                }
            }
        }
    }
}

const ABI: ProgramAbi = ProgramAbi {
    instance_stride: 16,
    uniform_size: 16,
    textures: 0,
    samplers: 0,
    vertex_textures: false,
};

fn desc(code: &ProgramCode) -> ProgramDesc<'_> {
    ProgramDesc {
        label: "fill",
        code,
        vertex_entry: "viso_vertex",
        fragment_entry: "viso_fragment",
        color_format: TextureFormat::Bgra8Unorm,
        blend: BlendMode::PremultipliedOver,
        abi: ABI,
    }
}

fn floats(v: &[f32]) -> Vec<u8> {
    v.iter().flat_map(|f| f.to_le_bytes()).collect()
}

#[test]
fn a_program_draws_its_instances_from_the_offset_inside_the_scissor() {
    let mut gpu = HeadlessRaster::new();
    let (w, h) = (32u32, 16u32);
    let surface = gpu.create_surface(RawWindowHandle::Headless, w, h);
    let code = ProgramCode::Cpu(Arc::new(FillRects));
    // Compiled where a worker would, installed on the render thread.
    let compiler = gpu.program_compiler();
    let program = std::thread::spawn(move || compiler.compile(&desc(&code)))
        .join()
        .unwrap()
        .expect("compiles");
    let pipeline = gpu.install_program(program);

    // A rect the draw skips (before the offset), then two it draws.
    let bytes = floats(&[
        0.0, 0.0, 32.0, 16.0, 0.0, 0.0, 8.0, 8.0, 20.0, 4.0, 12.0, 12.0,
    ]);
    let instances = gpu.create_buffer(&BufferDesc {
        size: bytes.len(),
        usage: BufferUsage::INSTANCE | BufferUsage::CPU_WRITE,
        label: "rects",
    });
    gpu.write_buffer(instances, 0, &bytes);

    let frame = gpu.begin_frame(surface).expect("acquires");
    let red = floats(&[1.0, 0.0, 0.0, 1.0]);
    gpu.encode(&DrawList {
        commands: &[DrawCommand {
            pipeline,
            bind_group: None,
            geometry: Geometry::Generated { count: 2 },
            instance_buffer: instances,
            instance_offset: 16,
            uniforms: InlineUniforms::new(&red),
            scissor: Some((0, 0, 28, 16)),
        }],
        passes: &[RenderPass {
            target: RenderTarget::Surface(frame),
            load: LoadOp::Clear([0.0, 0.0, 1.0, 1.0]),
            first_command: 0,
            command_count: 1,
        }],
    });
    gpu.present(frame);

    let is_red = |x, y| gpu.surface_texel(surface, x, y) == [1.0, 0.0, 0.0, 1.0];
    assert!(is_red(4, 4), "the first drawn rect");
    assert!(is_red(24, 8), "the second drawn rect");
    assert!(!is_red(12, 12), "the rect before the offset is not drawn");
    assert!(!is_red(30, 8), "the scissor clips the second rect");

    gpu.destroy_pipeline(pipeline);
    let frame = gpu.begin_frame(surface).expect("acquires");
    gpu.present(frame);
}

#[test]
fn the_headless_compiler_takes_cpu_programs_within_the_binding_limits() {
    let gpu = HeadlessRaster::new();
    let compiler = gpu.program_compiler();
    let msl = ProgramCode::Msl(String::new());
    let log = compiler
        .compile(&desc(&msl))
        .err()
        .expect("not a CPU program");
    assert!(log.contains("CPU programs"), "{log}");

    let code = ProgramCode::Cpu(Arc::new(FillRects));
    let mut wide = desc(&code);
    wide.abi.textures = viso_gpu::PROGRAM_TEXTURES + 1;
    let log = compiler.compile(&wide).err().expect("too many textures");
    assert!(log.contains("at most"), "{log}");
}
