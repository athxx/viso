//! Shader declarations checked against the closed type set, the shader
//! subset and the render profile's stage rules, and lowered into a
//! `viso_shader` program: `E8101`–`E8106` and the shared type codes.

use viso_dsl::frontend::{Origin, compile_file};
use viso_shader::program::{ExprKind, Program, Stage, Stmt, Ty};

fn origin() -> Origin {
    Origin {
        package: "app".to_owned(),
        module: vec!["main".to_owned()],
        language: None,
    }
}

/// The errors compiling `source` raises, by code and message.
fn errors(source: &str) -> Vec<(String, String)> {
    compile_file(source, &origin())
        .errors()
        .map(|d| (d.code.to_string(), d.message.clone()))
        .collect()
}

fn codes(source: &str) -> Vec<String> {
    errors(source).into_iter().map(|(c, _)| c).collect()
}

/// The one shader `source` declares, checked cleanly.
fn program(source: &str) -> Program {
    let compiled = compile_file(source, &origin());
    let errors: Vec<_> = compiled.errors().collect();
    assert!(errors.is_empty(), "{errors:#?}");
    let [shader] = compiled.shaders.as_slice() else {
        panic!("one shader");
    };
    let program = shader.program.clone().expect("a program");
    program.validate().expect("valid");
    program
}

/// `body` as the fragment of a minimal shader with `members`.
fn fragment(members: &str, body: &str) -> String {
    format!(
        "shader S {{
    {members}
    vertex(vertex_id: U32) -> VertexOutput {{
        return VertexOutput {{ clip_position: Vec4F32(0.0, 0.0, 0.0, 1.0) }};
    }}
    fragment() -> Vec4F32 {{
        {body}
    }}
}}"
    )
}

/// The spec's example shader.
const ROUNDED_RECT: &str = r#"
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

#[test]
fn the_spec_example_lowers_to_a_valid_program() {
    let p = program(ROUNDED_RECT);
    assert_eq!(&*p.name, "RoundedRect");
    let names = |b: &[viso_shader::program::Binding]| -> Vec<(String, Ty)> {
        b.iter().map(|b| (b.name.to_string(), b.ty)).collect()
    };
    assert_eq!(names(&p.uniforms), [("viewport_size".to_owned(), Ty::VEC2)]);
    assert_eq!(
        names(&p.instance),
        [
            ("rect_pos".to_owned(), Ty::VEC2),
            ("rect_size".to_owned(), Ty::VEC2),
            ("radius".to_owned(), Ty::F32),
            ("color".to_owned(), Ty::Color),
        ]
    );
    assert_eq!(names(&p.varyings), [("local_pos".to_owned(), Ty::VEC2)]);
    let vertex = p.vertex.as_ref().expect("vertex");
    assert_eq!(
        (vertex.stage, vertex.ret, vertex.params),
        (Some(Stage::Vertex), Ty::VertexOutput, 1)
    );
    let fragment = p.fragment.as_ref().expect("fragment");
    assert_eq!(fragment.ret, Ty::VEC4);
    // `1.0f32` and `0.0f32` are typed literals; `alpha` is an `F32`.
    assert_eq!(fragment.locals[1].ty, Ty::F32);
}

#[test]
fn literals_take_their_type_from_context() {
    let p = program(&fragment(
        "uniform n: U32; uniform scale: F32;",
        "let a = scale * 2;
        let b = 2 * scale;
        let c = n + 1;
        let d = -3;
        let e = 0.5;
        let v = Vec4F32(1, 0.5, 0, 1);
        return v * a * b * e * (c as F32) * (d as F32);",
    ));
    let f = p.fragment.unwrap();
    let tys: Vec<Ty> = f.locals.iter().map(|l| l.ty).collect();
    assert_eq!(
        &tys[..6],
        [Ty::F32, Ty::F32, Ty::U32, Ty::I32, Ty::F32, Ty::VEC4]
    );
    let Stmt::Let(_, init) = &f.body.0[3] else {
        panic!("a let");
    };
    assert_eq!(init.kind, ExprKind::I32(-3));
}

#[test]
fn if_and_match_values_lower_into_assigned_locals() {
    let p = program(&fragment(
        "uniform mode: I32;",
        "let k = match mode { 0 | 1 => 0.25, 2 => 0.5, _ => 1.0 };
        let g = if k > 0.4 { k } else { 0.0 };
        match mode { 3 => { return Vec4F32(0.0); }, _ => {}, }
        return Vec4F32(g, g, g, 1.0);",
    ));
    let f = p.fragment.unwrap();
    let declares = f
        .body
        .0
        .iter()
        .filter(|s| matches!(s, Stmt::Declare(_)))
        .count();
    assert_eq!(declares, 2, "{:#?}", f.body);
}

#[test]
fn a_bounded_loop_is_literal_or_marked() {
    let p = program(
        &fragment(
            "uniform count: I32;",
            "var_free();
        return Vec4F32(0.0);",
        )
        .replace(
            "var_free();",
            "let mut sum = 0.0;
        for i in 0..4 { sum += 1.0; }
        @max_iterations(16)
        for j in 0..count { if j > 3 { break; } sum += 0.5; }
        let x = sum;",
        ),
    );
    let loops: Vec<(u32, bool)> = p
        .fragment
        .unwrap()
        .body
        .0
        .iter()
        .filter_map(|s| match s {
            Stmt::For(l) => Some((l.max, l.guarded)),
            _ => None,
        })
        .collect();
    assert_eq!(loops, [(4, false), (16, true)]);

    let unbounded = fragment(
        "uniform count: I32;",
        "for i in 0..count { } return Vec4F32(0.0);",
    );
    assert_eq!(codes(&unbounded), ["E8103"]);
    let while_ = fragment("", "while true { } return Vec4F32(0.0);");
    assert_eq!(codes(&while_), ["E8103"]);
    let misplaced = fragment("", "@max_iterations(4) let a = 1.0; return Vec4F32(a);");
    assert_eq!(codes(&misplaced), ["E8105"]);
}

#[test]
fn shader_value_records_are_the_only_records_a_shader_uses() {
    let source = r#"
@shader_value
record Light { position: Vec2F32; strength: F32; }

record Plain { x: F32; }

shader S {
    uniform light: Light;
    instance tint: ColorLinear;
    vertex(vertex_id: U32) -> VertexOutput {
        return VertexOutput { clip_position: Vec4F32(light.position, 0.0, 1.0) };
    }
    fragment() -> Vec4F32 {
        let l = Light { position: light.position, strength: light.strength * 2.0 };
        return tint.to_vec4() * l.strength;
    }
}
"#;
    let p = program(source);
    assert_eq!(p.records.len(), 1);
    assert_eq!(&*p.records[0].name, "Light");
    assert_eq!(p.uniforms[0].ty, Ty::Record(0));

    let plain = source.replace(
        "uniform light: Light;",
        "uniform light: Light; uniform plain: Plain;",
    );
    let e = errors(&plain);
    assert_eq!(e.len(), 1, "{e:?}");
    assert_eq!(e[0].0, "E8101");
    assert!(e[0].1.contains("not a `@shader_value` record"), "{e:?}");

    let with = |decl: &str| format!("{decl}\n{}", fragment("", "return Vec4F32(0.0);"));
    assert_eq!(
        codes(&with("@shader_value record R { name: String; }")),
        ["E8101"]
    );
    assert_eq!(
        codes(&with("@shader_value record R { x: F64; }")),
        ["E8102"]
    );
    assert_eq!(
        codes(&with("@shader_value record R { x: F32 = 1.0; }")),
        ["E8105"]
    );
    assert_eq!(
        codes(&with("@shader_value record R { inner: R; }")),
        ["E8105"]
    );
    assert_eq!(codes(&with("@shader_value const X: I64 = 1;")), ["E8105"]);
}

#[test]
fn host_types_and_f64_stay_out_of_a_shader() {
    assert_eq!(
        codes(&fragment("uniform name: String;", "return Vec4F32(0.0);")),
        ["E8101"]
    );
    assert_eq!(
        codes(&fragment("uniform x: F64;", "return Vec4F32(0.0);")),
        ["E8102"]
    );
    assert_eq!(
        codes(&fragment("", "let x = 1.0f64; return Vec4F32(0.0);")),
        ["E8102"]
    );
    assert_eq!(
        codes(&fragment("", "let s = \"a\"; return Vec4F32(0.0);")),
        ["E8101"]
    );
    assert_eq!(
        codes(&fragment("", "let c = #ff0000; return Vec4F32(0.0);")),
        ["E8101"]
    );
    assert_eq!(
        codes(&fragment("", "let l = [1.0]; return Vec4F32(0.0);")),
        ["E8101"]
    );
    assert_eq!(
        codes(&fragment("uniform x: Bogus;", "return Vec4F32(0.0);")),
        ["E2001"]
    );
}

#[test]
fn a_bool_has_no_buffer_layout() {
    assert_eq!(
        codes(&fragment("instance on: Bool;", "return Vec4F32(0.0);")),
        ["E8104"]
    );
    assert_eq!(
        codes(&fragment("uniform on: Vec2U32;", "return Vec4F32(0.0);")),
        Vec::<String>::new()
    );
}

#[test]
fn the_subset_rejects_what_a_gpu_cannot_run() {
    let recursive = "shader S {
    fn a(x: F32) -> F32 { return b(x); }
    fn b(x: F32) -> F32 { return a(x); }
    vertex(vertex_id: U32) -> VertexOutput { return VertexOutput { clip_position: Vec4F32(a(1.0)) }; }
    fragment() -> Vec4F32 { return Vec4F32(1.0); }
}";
    assert_eq!(codes(recursive), ["E8105"]);
    assert_eq!(
        codes(&fragment("", "let f = |x: F32| x; return Vec4F32(0.0);")),
        ["E8105"]
    );
    assert_eq!(
        codes(&fragment("", "return Vec4F32(f(b: 1.0));")),
        ["E8105"]
    );
    assert_eq!(codes("shader S<T> { }"), ["E8105"]);
    assert_eq!(
        codes(&fragment("", "return Vec4F32(missing(1.0));")),
        ["E2001"]
    );
}

#[test]
fn stage_rules_follow_the_render_profile() {
    let varying_write = fragment("varying v: F32;", "v = 1.0; return Vec4F32(v);");
    assert_eq!(codes(&varying_write), ["E8106"]);
    let derivative = "shader S {
    vertex(vertex_id: U32) -> VertexOutput { return VertexOutput { clip_position: Vec4F32(dpdx(1.0)) }; }
    fragment() -> Vec4F32 { return Vec4F32(1.0); }
}";
    assert_eq!(codes(derivative), ["E8106"]);
    let builtin = "shader S {
    vertex(pixel: U32) -> VertexOutput { return VertexOutput { clip_position: Vec4F32(1.0) }; }
    fragment() -> Vec4F32 { return Vec4F32(1.0); }
}";
    assert_eq!(codes(builtin), ["E8106"]);
    assert_eq!(
        codes("shader S { fragment() -> Vec4F32 { return Vec4F32(1.0); } }"),
        ["E8106"]
    );
    let compute = "shader S {
    vertex(vertex_id: U32) -> VertexOutput { return VertexOutput { clip_position: Vec4F32(1.0) }; }
    fragment() -> Vec4F32 { return Vec4F32(1.0); }
    compute() -> U32 { return 0; }
}";
    assert_eq!(codes(compute), ["E8106"]);
    let reads_member = "shader S {
    uniform k: F32;
    fn f() -> F32 { return k; }
    vertex(vertex_id: U32) -> VertexOutput { return VertexOutput { clip_position: Vec4F32(f()) }; }
    fragment() -> Vec4F32 { return Vec4F32(1.0); }
}";
    assert_eq!(codes(reads_member), ["E8106"]);
}

#[test]
fn types_do_not_convert_implicitly() {
    let mixed = fragment("uniform n: I32; uniform x: F32;", "return Vec4F32(x + n);");
    assert_eq!(codes(&mixed), ["E2102"]);
    let mismatch = fragment("", "let v: Vec2F32 = Vec3F32(1.0); return Vec4F32(0.0);");
    assert_eq!(codes(&mismatch), ["E2103"]);
    let immutable = fragment("", "let a = 1.0; a = 2.0; return Vec4F32(a);");
    assert_eq!(codes(&immutable), ["E2110"]);
    let missing_return = fragment("", "let a = 1.0;");
    assert_eq!(codes(&missing_return), ["E2103"]);
    let partial = fragment(
        "uniform n: I32;",
        "let k = match n { 0 => 1.0 }; return Vec4F32(k);",
    );
    assert_eq!(codes(&partial), ["E2301"]);
}
