//! Shaders as brushes: a shader's name heads a record literal of its
//! parameters, typed against them; a `ShaderView`'s `shader` takes one; a
//! shader a view paints with keeps the brush profile (`E8107`), one that
//! paints nothing need not.

use viso_dsl::frontend::{Origin, compile_file};
use viso_shader::program::ParamKind;

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

/// A shader `name` with the reserved members, `members` and a fragment
/// returning its `tint`.
fn brush(name: &str, members: &str) -> String {
    format!(
        "shader {name} {{
    uniform viewport_size: Vec2F32;
    instance rect_pos: Vec2F32;
    instance rect_size: Vec2F32;
    {members}
    vertex(vertex_id: U32) -> VertexOutput {{
        let unit = quad_vertex(vertex_id);
        return VertexOutput {{
            clip_position: to_clip(rect_pos + unit * rect_size, viewport_size),
        }};
    }}
    fragment() -> Vec4F32 {{
        return Vec4F32(0.0, 0.0, 0.0, 1.0);
    }}
}}"
    )
}

const RIPPLE_MEMBERS: &str = "instance tint: ColorLinear;
    instance rings: F32;
    instance center: Vec2F32;
    instance seed: U32;";

/// A component painting `value` with a `ShaderView`, after `shaders`.
fn painting(shaders: &str, value: &str) -> String {
    format!(
        "import viso::math::Vec2F32;
{shaders}
export component Card {{
    state rings: F32 = 8.0;
    view {{
        ShaderView {{
            shader: {value};
            width: 120dp;
            height: 80dp;
        }}
    }}
}}"
    )
}

const RIPPLE: &str =
    "Ripple { tint: #3366ff, rings: rings, center: Vec2F32::new(0.5, 0.5), seed: 7 }";

#[test]
fn a_shader_name_heads_its_parameters_and_paints_a_shader_view() {
    let source = painting(&brush("Ripple", RIPPLE_MEMBERS), RIPPLE);
    let compiled = compile_file(&source, &origin());
    let errors: Vec<_> = compiled.errors().collect();
    assert!(errors.is_empty(), "{errors:#?}");
    let [shader] = compiled.shaders.as_slice() else {
        panic!("one shader");
    };
    let layout = shader.brush.as_ref().expect("a brush");
    let params: Vec<(&str, ParamKind)> = layout.params.iter().map(|p| (&*p.name, p.kind)).collect();
    assert_eq!(
        params,
        [
            ("tint", ParamKind::ColorLinear),
            ("rings", ParamKind::F32),
            ("center", ParamKind::Vec2F32),
            ("seed", ParamKind::U32),
        ]
    );
}

#[test]
fn a_parameter_is_typed_against_its_member() {
    let ripple = brush("Ripple", RIPPLE_MEMBERS);
    let wrong = "Ripple { tint: #3366ff, rings: \"eight\", center: Vec2F32::new(0.5, 0.5), \
                 seed: 7 }";
    assert_eq!(codes(&painting(&ripple, wrong)), ["E2103"]);
    let missing = "Ripple { tint: #3366ff, rings: rings, seed: 7 }";
    let found = errors(&painting(&ripple, missing));
    assert_eq!(found.len(), 1, "{found:?}");
    assert!(
        found[0].0 == "E2103" && found[0].1.contains("center"),
        "{found:?}"
    );
    let unknown = "Ripple { tint: #3366ff, rings: rings, center: Vec2F32::new(0.5, 0.5), \
                   seed: 7, speed: 1.0 }";
    assert_eq!(codes(&painting(&ripple, unknown)), ["E2001"]);
    // The box is the renderer's: it is no parameter.
    let boxed = "Ripple { tint: #3366ff, rings: rings, center: Vec2F32::new(0.5, 0.5), \
                 seed: 7, rect_pos: Vec2F32::new(0.0, 0.0) }";
    assert_eq!(codes(&painting(&ripple, boxed)), ["E2001"]);
}

#[test]
fn a_shader_view_takes_only_a_shaders_parameters() {
    let ripple = brush("Ripple", RIPPLE_MEMBERS);
    let found = errors(&painting(&ripple, "rings"));
    assert_eq!(found.len(), 1, "{found:?}");
    assert!(
        found[0].0 == "E2103" && found[0].1.contains("`shader` takes a shader's parameters"),
        "{found:?}"
    );
    let record = format!("record Point {{ x: F32; }}\n{ripple}");
    assert_eq!(codes(&painting(&record, "Point { x: 1.0 }")), ["E2103"]);
}

/// A shader breaking the brush profile every way: no `rect_size`, a uniform
/// the renderer does not write, a parameter of no host kind and a texture.
const OFF_PROFILE: &str = "shader Glow {
    uniform viewport_size: Vec2F32;
    uniform time: F32;
    instance rect_pos: Vec2F32;
    instance strength: F32;
    instance basis: Mat2F32;
    texture noise: Texture2D<Vec4F32>;
    vertex(vertex_id: U32) -> VertexOutput {
        let unit = quad_vertex(vertex_id);
        return VertexOutput {
            clip_position: to_clip(rect_pos + unit * basis[0] * time, viewport_size),
        };
    }
    fragment() -> Vec4F32 {
        return Vec4F32(strength, 0.0, 0.0, 1.0);
    }
}";

#[test]
fn a_shader_breaking_the_brush_profile_is_reported_only_where_it_paints() {
    let unused = format!("{OFF_PROFILE}\nexport component Empty {{ view {{ Text {{ }} }} }}");
    assert_eq!(codes(&unused), Vec::<String>::new());

    let source = painting(OFF_PROFILE, "Glow { strength: 0.5 }");
    let compiled = compile_file(&source, &origin());
    let found: Vec<_> = compiled.errors().collect();
    let found_codes: Vec<&str> = found.iter().map(|d| d.code).collect();
    assert_eq!(found_codes, ["E8107"; 4], "{found:#?}");
    let messages: Vec<&str> = found.iter().map(|d| d.message.as_str()).collect();
    for part in [
        "`time`",
        "`basis`",
        "`noise`",
        "instance rect_size: Vec2F32;",
    ] {
        assert!(
            messages.iter().any(|m| m.contains(part)),
            "{part}: {messages:#?}"
        );
    }
    let missing = found
        .iter()
        .find(|d| d.message.contains("rect_size"))
        .unwrap();
    let at = |range: viso_dsl::syntax::TextRange| {
        &source[range.start().to_u32() as usize..range.end().to_u32() as usize]
    };
    assert_eq!(
        at(missing.primary),
        "Glow",
        "a missing member points at the name"
    );
    for d in &found {
        let [related] = d.related.as_slice() else {
            panic!("{d:#?}");
        };
        assert_eq!(at(related.range), "Glow { strength: 0.5 }");
    }
    assert!(compiled.shaders[0].program.is_some());
    assert!(compiled.shaders[0].brush.is_none());

    // Two views painting with it report it once.
    let twice = format!(
        "{OFF_PROFILE}
export component Pair {{
    view {{
        Column {{
            ShaderView {{ shader: Glow {{ strength: 0.5 }}; }}
            ShaderView {{ shader: Glow {{ strength: 1.0 }}; }}
        }}
    }}
}}"
    );
    assert_eq!(codes(&twice), ["E8107"; 4]);
}
