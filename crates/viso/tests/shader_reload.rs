//! A `.vs` shader edited while it draws: the worker compiles each edit
//! through the DSL front end and Metal, a failure keeps the live pipeline
//! with the front end's spans, and a success swaps pipeline and instance
//! buffer together at the frame boundary.

#![cfg(target_os = "macos")]

use std::time::{Duration, Instant};

use viso_dsl::frontend::{Origin, compile_file};
use viso_gpu::{
    BlendMode, BufferDesc, BufferUsage, GpuBackend, MetalBackend, MetalCompiler, MetalProgram,
    ProgramDesc, TextureFormat,
};
use viso_shader::program::{
    FRAGMENT_ENTRY, Program, ProgramError, ProgramReload, ReloadError, ReloadEvent,
    ShaderInterface, Span, Target, VERTEX_ENTRY, Value, emit,
};

fn lower(source: &str) -> Result<Program, Vec<ProgramError>> {
    let origin = Origin {
        package: "app".to_owned(),
        module: vec!["main".to_owned()],
        language: None,
    };
    let compiled = compile_file(source, &origin);
    let errors: Vec<ProgramError> = compiled
        .errors()
        .map(|d| {
            let span = Span {
                start: d.primary.start().to_u32(),
                end: d.primary.end().to_u32(),
            };
            ProgramError::new(d.code, span, d.message.clone())
        })
        .collect();
    if !errors.is_empty() {
        return Err(errors);
    }
    let shader = compiled.shaders.into_iter().next().expect("one shader");
    Ok(shader.program.expect("a checked shader lowers"))
}

fn compiler(
    c: MetalCompiler,
) -> impl FnMut(&Program, &ShaderInterface) -> Result<MetalProgram, String> {
    move |p, interface| {
        let msl = emit(p, interface, Target::Msl);
        c.compile(&ProgramDesc {
            label: &p.name,
            msl: &msl,
            vertex_entry: VERTEX_ENTRY,
            fragment_entry: FRAGMENT_ENTRY,
            color_format: TextureFormat::Bgra8Unorm,
            blend: BlendMode::PremultipliedOver,
            vertex_textures: interface.vertex_textures,
        })
    }
}

fn source(instance: &str, tint: &str) -> String {
    format!(
        r#"
shader Dot {{
    uniform viewport: Vec2F32;
    {instance}
    varying local: Vec2F32;

    vertex(vertex_id: U32) -> VertexOutput {{
        let unit = quad_vertex(vertex_id);
        local = unit * size;
        return VertexOutput {{ clip_position: to_clip(pos + local, viewport) }};
    }}

    fragment() -> Vec4F32 {{
        let d = rounded_rect_sdf(local, size, 4.0);
        return {tint} * smoothstep(1.0, 0.0, d);
    }}
}}
"#
    )
}

fn wait(r: &mut ProgramReload<String, MetalProgram>) -> ReloadEvent<MetalProgram> {
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        if let Some(event) = r.poll() {
            return event;
        }
        assert!(Instant::now() < deadline, "no reload outcome");
        std::thread::sleep(Duration::from_millis(2));
    }
}

#[test]
fn an_edit_compiles_off_thread_and_swaps_with_its_instance_buffer() {
    let mut backend = MetalBackend::new();
    let first = source(
        "instance pos: Vec2F32; instance size: Vec2F32;",
        "Vec4F32(1.0)",
    );
    let mut reload = ProgramReload::new(first, |s: &String| lower(s), compiler(backend.compiler()))
        .expect("builds");
    let mut pipeline = backend.install_program(&reload.live().pipeline);
    let compiles = backend.library_compiles();

    // The host's instances, encoded through the live layout.
    let live = reload.live();
    let rows = [
        [Value::floats(&[10.0, 20.0]), Value::floats(&[30.0, 40.0])],
        [Value::floats(&[50.0, 60.0]), Value::floats(&[70.0, 80.0])],
    ];
    let mut bytes = Vec::new();
    for row in &rows {
        bytes.extend(
            live.interface
                .instance
                .encode(&live.program.instance, row)
                .expect("encodes"),
        );
    }
    let buffer_desc = |size: usize| BufferDesc {
        size,
        usage: BufferUsage::INSTANCE,
        label: "dots",
    };
    let mut buffer = backend.create_buffer(&buffer_desc(bytes.len()));
    backend.write_buffer(buffer, 0, &bytes);

    // A type error: the front end's span comes back, the pipeline stays.
    let broken = source(
        "instance pos: Vec2F32; instance size: Vec2F32;",
        "Vec3F32(1.0)",
    );
    let at = broken.find("Vec3F32(1.0)").unwrap() as u32;
    reload.submit(broken);
    let ReloadEvent::Failed(ReloadError::Program(errors)) = wait(&mut reload) else {
        panic!("a front-end failure");
    };
    assert!(
        errors
            .iter()
            .any(|e| e.span.start <= at && at < e.span.end.max(at + 1)),
        "{errors:?} should point near byte {at}"
    );
    assert_eq!(
        backend.library_compiles(),
        compiles,
        "nothing reached Metal"
    );

    // A new instance member: pipeline and re-encoded buffer go live together.
    let edited = source(
        "instance tint: ColorLinear; instance pos: Vec2F32; instance size: Vec2F32;",
        "tint.to_vec4()",
    );
    reload.submit(edited);
    let ReloadEvent::Swapped(swap) = wait(&mut reload) else {
        panic!("a swap");
    };
    assert_eq!(
        backend.library_compiles(),
        compiles + 1,
        "compiled on the worker"
    );
    let migration = swap.instance.expect("the instance layout changed");
    let moved = migration.reencode(&bytes);
    let next = backend.install_program(&reload.live().pipeline);
    let next_buffer = backend.create_buffer(&buffer_desc(moved.len()));
    backend.write_buffer(next_buffer, 0, &moved);
    backend.destroy_pipeline(pipeline);
    backend.destroy_buffer(buffer);
    (pipeline, buffer) = (next, next_buffer);
    drop(swap.retired);

    let live = reload.live();
    let stride = live.interface.instance.size as usize;
    assert_eq!(stride, 32);
    let second =
        live.interface
            .instance
            .decode(&live.program, &live.program.instance, &moved[stride..]);
    assert_eq!(second[1..], rows[1]);
    assert_eq!(second[0], Value::floats(&[0.0; 4]));
    let _ = (pipeline, buffer);
}
