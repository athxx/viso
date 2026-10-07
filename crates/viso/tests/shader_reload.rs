//! A `.vs` shader edited while it draws: the worker compiles each edit
//! through the DSL front end and Metal, a failure keeps the live pipeline
//! with the front end's spans, and a success swaps pipeline and instance
//! buffer together at the frame boundary.

#![cfg(target_os = "macos")]

use std::time::{Duration, Instant};

use viso_dsl::frontend::{Origin, compile_file};
use viso_gpu::{
    BindGroupDesc, Binding, BlendMode, BufferDesc, BufferUsage, DrawCommand, DrawList, Geometry,
    GpuBackend, InlineUniforms, LoadOp, MetalBackend, MetalCompiler, MetalProgram, PipelineId,
    ProgramDesc, RenderPass, RenderTarget, TextureDesc, TextureFormat,
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

/// Draws two dots with `pipeline` from the live layout's encoding of the
/// same rows, returning the BGRA8 picture.
fn draw_dots(
    backend: &mut MetalBackend,
    pipeline: PipelineId,
    reload: &ProgramReload<String, MetalProgram>,
) -> Vec<u8> {
    let live = reload.live();
    let uniforms = live
        .interface
        .uniforms
        .encode(&live.program.uniforms, &[Value::floats(&[64.0, 64.0])])
        .expect("uniforms encode");
    let mut instances = Vec::new();
    for row in [
        [Value::floats(&[4.0, 4.0]), Value::floats(&[24.0, 20.0])],
        [Value::floats(&[30.0, 28.0]), Value::floats(&[28.0, 30.0])],
    ] {
        instances.extend(
            live.interface
                .instance
                .encode(&live.program.instance, &row)
                .expect("encodes"),
        );
    }
    let buffer = |backend: &mut MetalBackend, bytes: &[u8], usage| {
        let id = backend.create_buffer(&BufferDesc {
            size: bytes.len().max(16),
            usage,
            label: "dots",
        });
        backend.write_buffer(id, 0, bytes);
        id
    };
    let uniform_buffer = buffer(backend, &uniforms, BufferUsage::UNIFORM);
    let instance_buffer = buffer(backend, &instances, BufferUsage::INSTANCE);
    let bind_group = backend.create_bind_group(&BindGroupDesc {
        label: "dots",
        bindings: vec![Binding::Uniform(uniform_buffer)],
    });
    let target = backend.create_texture(&TextureDesc {
        width: 64,
        height: 64,
        format: TextureFormat::Bgra8Unorm,
        render_target: true,
        label: "dots-target",
    });
    let commands = [DrawCommand {
        pipeline,
        bind_group: Some(bind_group),
        geometry: Geometry::Generated { count: 2 },
        instance_buffer,
        instance_offset: 0,
        uniforms: InlineUniforms::new(&[]),
        scissor: None,
    }];
    let passes = [RenderPass {
        target: RenderTarget::Texture(target),
        load: LoadOp::Clear([0.0, 0.0, 0.0, 1.0]),
        first_command: 0,
        command_count: 1,
    }];
    backend.encode(&DrawList {
        commands: &commands,
        passes: &passes,
    });
    backend.read_texture(target)
}

#[test]
fn a_backend_rejection_keeps_the_pipeline_that_draws() {
    let mut backend = MetalBackend::new();
    // The worker's compiler is Metal's; a shader named `Rejected` reaches it
    // with a line Metal cannot compile, standing for any source the front
    // end accepts and the backend does not.
    let metal = backend.compiler();
    let compile = move |p: &Program, interface: &ShaderInterface| {
        let mut msl = emit(p, interface, Target::Msl);
        if &*p.name == "Rejected" {
            msl.push_str("\nthis is not metal;\n");
        }
        metal.compile(&ProgramDesc {
            label: &p.name,
            msl: &msl,
            vertex_entry: VERTEX_ENTRY,
            fragment_entry: FRAGMENT_ENTRY,
            color_format: TextureFormat::Bgra8Unorm,
            blend: BlendMode::PremultipliedOver,
            vertex_textures: interface.vertex_textures,
        })
    };
    let first = source(
        "instance pos: Vec2F32; instance size: Vec2F32;",
        "Vec4F32(1.0, 0.5, 0.25, 1.0)",
    );
    let mut reload =
        ProgramReload::new(first.clone(), |s: &String| lower(s), compile).expect("builds");
    let pipeline = backend.install_program(&reload.live().pipeline);
    let before = draw_dots(&mut backend, pipeline, &reload);
    let lit = before
        .as_chunks::<4>()
        .0
        .iter()
        .filter(|p| p[..3] != [0, 0, 0])
        .count();
    assert!(lit > 400, "the dots are drawn: {lit} pixels");

    reload.submit(first.replace("shader Dot", "shader Rejected"));
    let ReloadEvent::Failed(ReloadError::Backend(log)) = wait(&mut reload) else {
        panic!("a backend failure");
    };
    assert!(!log.is_empty(), "Metal's log comes back");
    assert_eq!(
        &*reload.live().program.name,
        "Dot",
        "the live program stays"
    );
    let after = draw_dots(&mut backend, pipeline, &reload);
    assert_eq!(after, before, "the kept pipeline draws as before");
}
