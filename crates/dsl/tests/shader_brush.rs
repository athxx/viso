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
        ..Default::default()
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

/// A shader breaking the brush profile every way: a uniform the renderer
/// does not write, a parameter of no host kind and a texture of no image.
const OFF_PROFILE: &str = "shader Glow {
    uniform scale: F32;
    instance strength: F32;
    instance basis: Mat2F32;
    texture noise: Texture2D<F32>;
    vertex(vertex_id: U32) -> VertexOutput {
        let unit = quad_vertex(vertex_id);
        return VertexOutput {
            clip_position: to_clip(rect_pos + unit * basis[0] * scale, viewport_size),
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
    assert_eq!(found_codes, ["E8107"; 3], "{found:#?}");
    let messages: Vec<&str> = found.iter().map(|d| d.message.as_str()).collect();
    for part in ["`scale`", "`basis`", "`noise`"] {
        assert!(
            messages.iter().any(|m| m.contains(part)),
            "{part}: {messages:#?}"
        );
    }
    let at = |range: viso_dsl::syntax::TextRange| {
        &source[range.start().to_u32() as usize..range.end().to_u32() as usize]
    };
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
    assert_eq!(codes(&twice), ["E8107"; 3]);
}

/// A shader using every construct a program holds: a record, a `fn`, a
/// varying, the fragment's builtin, a loop, branches, swizzles, an index, a
/// conversion and a discard.
const EVERY: &str = "@shader_value
record Light { position: Vec2F32; strength: F32; }

shader Every {
    uniform light: Light;
    uniform mode: I32;
    instance tint: ColorLinear;
    varying local: Vec2F32;
    varying band: U32;

    fn falloff(d: F32, k: F32) -> F32 {
        return clamp(1.0 - d * k, 0.0, 1.0);
    }

    vertex(vertex_id: U32, instance_id: U32) -> VertexOutput {
        let unit = quad_vertex(vertex_id);
        local = unit * rect_size;
        band = instance_id % 3u32;
        return VertexOutput {
            clip_position: to_clip(rect_pos + local, viewport_size),
        };
    }

    fragment(frag_coord: Vec4F32) -> Vec4F32 {
        let mut sum = 0.0;
        for i in 0..4 { sum += falloff(distance(local, light.position), 0.01) * 0.25; }
        let k = match mode { 0 | 1 => 0.25, 2 => 0.5, _ => 1.0 };
        let g = if k > 0.4 { k } else { -k };
        if frag_coord.x < 0.0 { discard(); }
        let c = tint.to_vec4();
        let lane = c[band];
        let rgb = c.rgb * sum * light.strength;
        let flip = if !(mode == 1) { 1.0 } else { 0.0 };
        let n = (band as F32) + lane + g + flip;
        return Vec4F32(rgb, c.a * fract(n));
    }
}";

#[test]
fn a_program_and_a_packed_brush_round_trip_through_the_package_codec() {
    use viso_ende::{Decode, Encode};
    use viso_shader::program::{PackedBrush, PackedCode, Program, ShaderLang};

    let compiled = compile_file(EVERY, &origin());
    let errors: Vec<_> = compiled.errors().collect();
    assert!(errors.is_empty(), "{errors:#?}");
    let program = compiled.shaders[0].program.clone().expect("a program");
    let bytes = program.encode_to_vec();
    let decoded = Program::decode_from_slice(&bytes).unwrap();
    assert_eq!(decoded.encode_to_vec(), bytes);
    let msl = |p: &Program| match viso_shader::program::code(p, &p.interface(), ShaderLang::Msl) {
        Ok(viso_shader::program::ProgramCode::Msl(text)) => text,
        other => panic!("{other:?}"),
    };
    assert_eq!(
        msl(&decoded),
        msl(&program),
        "the same program but its spans"
    );
    assert!(
        decoded
            .functions
            .iter()
            .all(|f| f.span == Default::default())
    );

    let source = painting(&brush("Ripple", RIPPLE_MEMBERS), RIPPLE);
    let compiled = compile_file(&source, &origin());
    let shader = &compiled.shaders[0];
    let (program, layout) = (
        shader.program.as_ref().unwrap(),
        shader.brush.clone().unwrap(),
    );
    for lang in [
        ShaderLang::None,
        ShaderLang::Msl,
        ShaderLang::Wgsl,
        ShaderLang::Hlsl,
        ShaderLang::SpirV,
    ] {
        let packed = PackedBrush::pack(program, layout.clone(), &[lang]).unwrap();
        assert_eq!(packed.langs().collect::<Vec<_>>(), [lang]);
        let bytes = packed.encode_to_vec();
        assert_eq!(PackedBrush::decode_from_slice(&bytes).unwrap(), packed);
        let code = packed.unpack();
        assert_eq!(code.iter().map(|c| c.lang()).collect::<Vec<_>>(), [lang]);
        // Every truncation is an error, never a panic.
        for end in 0..bytes.len() {
            assert!(PackedBrush::decode_from_slice(&bytes[..end]).is_err());
        }
    }

    // A release package's brush: the target's language and the program, each
    // once.
    let langs = [ShaderLang::Msl, ShaderLang::None, ShaderLang::Msl];
    let packed = PackedBrush::pack(program, layout.clone(), &langs).unwrap();
    assert_eq!(
        packed.langs().collect::<Vec<_>>(),
        [ShaderLang::Msl, ShaderLang::None]
    );
    assert_eq!(
        PackedBrush::decode_from_slice(&packed.encode_to_vec()).unwrap(),
        packed
    );
    let mut twice = packed.clone();
    twice.code.push(twice.code[0].clone());
    assert!(PackedBrush::decode_from_slice(&twice.encode_to_vec()).is_err());

    // The brush moved down its file by an edit above it packs to the same
    // bytes: a package carries no span.
    let moved = compile_file(&format!("// a note\n\n{source}"), &origin());
    let shader = &moved.shaders[0];
    assert_ne!(shader.brush.as_ref().unwrap(), &layout, "the spans moved");
    let repacked = PackedBrush::pack(
        shader.program.as_ref().unwrap(),
        shader.brush.clone().unwrap(),
        &langs,
    )
    .unwrap();
    assert_eq!(repacked.encode_to_vec(), packed.encode_to_vec());

    // A CPU brush whose layout is not its program's own is refused.
    let mut packed = PackedBrush::pack(program, layout, &[ShaderLang::None]).unwrap();
    packed.layout.params[0].offset += 4;
    assert!(PackedBrush::decode_from_slice(&packed.encode_to_vec()).is_err());
    let PackedCode::Program(p) = &mut packed.code[0] else {
        unreachable!()
    };
    p.instance[0].ty = viso_shader::program::Ty::Color;
    assert!(PackedBrush::decode_from_slice(&packed.encode_to_vec()).is_err());
}

/// A brush sampling an image its node names through samplers it declares.
const PHOTO: &str = "shader Photo {
    instance fade: F32;
    texture photo: Texture2D<Vec4F32>;
    @sampler(filter: nearest, address: repeat)
    sampler pixels: Sampler;
    sampler smooth: Sampler;
    varying uv: Vec2F32;
    vertex(vertex_id: U32) -> VertexOutput {
        let unit = quad_vertex(vertex_id);
        uv = unit;
        return VertexOutput {
            clip_position: to_clip(rect_pos + unit * rect_size, viewport_size),
        };
    }
    fragment() -> Vec4F32 {
        return sample(photo, pixels, uv) * fade + sample(photo, smooth, uv) * (1.0 - fade);
    }
}";

#[test]
fn a_brush_samples_its_nodes_images_through_samplers_it_declares() {
    use viso_shader::program::{AddressMode, FilterMode, SamplerDesc};

    let compiled = compile_file(PHOTO, &origin());
    let errors: Vec<_> = compiled.errors().collect();
    assert!(errors.is_empty(), "{errors:#?}");
    let layout = compiled.shaders[0].brush.as_ref().expect("a brush");
    let textures: Vec<&str> = layout.textures.iter().map(|t| &*t.name).collect();
    assert_eq!(textures, ["photo"]);
    assert_eq!(
        layout.samplers,
        [
            SamplerDesc {
                filter: FilterMode::Nearest,
                address: AddressMode::Repeat,
            },
            SamplerDesc::LINEAR_CLAMP,
        ]
    );
    let abi = layout.abi();
    assert_eq!((abi.textures, abi.samplers), (1, 2));

    // Every backend's code binds the image and both samplers, and the
    // package carries the samplers' state.
    use viso_ende::{Decode, Encode};
    use viso_shader::program::{PackedBrush, ShaderLang};
    let program = compiled.shaders[0].program.as_ref().unwrap();
    for lang in [
        ShaderLang::None,
        ShaderLang::Msl,
        ShaderLang::Wgsl,
        ShaderLang::Hlsl,
        ShaderLang::SpirV,
    ] {
        let packed = PackedBrush::pack(program, layout.clone(), &[lang]).unwrap();
        let decoded = PackedBrush::decode_from_slice(&packed.encode_to_vec()).unwrap();
        assert_eq!(decoded, packed, "{lang:?}");
    }
    let mut packed = PackedBrush::pack(program, layout.clone(), &[ShaderLang::None]).unwrap();
    packed.layout.textures.clear();
    assert!(
        PackedBrush::decode_from_slice(&packed.encode_to_vec()).is_err(),
        "a layout missing the program's texture is not its own"
    );
}

#[test]
fn a_sampler_setting_is_checked() {
    let with = |attr: &str, member: &str| {
        let shader = PHOTO.replace(
            "@sampler(filter: nearest, address: repeat)\n    sampler pixels: Sampler;",
            &format!("{attr}\n    {member}"),
        );
        errors(&shader)
    };
    let sampler = "sampler pixels: Sampler;";
    let cases = [
        ("@sampler(filter: linear)", sampler, None),
        (
            "@sampler(filter: cubic)",
            sampler,
            Some("`nearest` or `linear`"),
        ),
        (
            "@sampler(address: wrap)",
            sampler,
            Some("`clamp`, `repeat` or `mirror`"),
        ),
        (
            "@sampler(blend: nearest)",
            sampler,
            Some("`blend` is no sampler setting"),
        ),
        (
            "@sampler(nearest)",
            sampler,
            Some("`filter: …` or `address: …`"),
        ),
        (
            "@sampler(filter: nearest, filter: linear)",
            sampler,
            Some("`filter` is set twice"),
        ),
        (
            "@sampler(filter: nearest) @sampler(address: repeat)",
            sampler,
            Some("one `@sampler`"),
        ),
        (
            "@sampler(filter: nearest)",
            "sampler pixels: Sampler;\n    @sampler(filter: nearest) uniform scale: F32;",
            Some("sets a `sampler` member's state"),
        ),
    ];
    for (attr, member, expected) in cases {
        let found = with(attr, member);
        match expected {
            None => assert!(found.is_empty(), "{attr}: {found:?}"),
            Some(part) => assert!(
                found.len() == 1 && found[0].0 == "E8105" && found[0].1.contains(part),
                "{attr}: {found:?}"
            ),
        }
    }
}

/// A package directory holding a photo and an icon, removed on drop.
struct Assets(std::path::PathBuf);

impl Assets {
    fn new(name: &str) -> Assets {
        let dir = std::env::temp_dir().join(format!("viso-brush-{name}-{}", std::process::id()));
        for (path, bytes) in [
            ("photo.png", &b"\x89PNG\r\n\x1a\nphoto"[..]),
            (
                "icon.svg",
                &b"<svg xmlns=\"http://www.w3.org/2000/svg\" width=\"8\" height=\"8\"/>"[..],
            ),
        ] {
            std::fs::create_dir_all(&dir).unwrap();
            std::fs::write(dir.join(path), bytes).unwrap();
        }
        Assets(dir)
    }

    fn errors(&self, source: &str) -> Vec<(String, String)> {
        let profile = viso_dsl::hir::TargetProfile {
            assets: Some(std::rc::Rc::new(
                viso_dsl::assets::AssetCatalog::scan(&self.0).unwrap(),
            )),
            ..viso_dsl::hir::TargetProfile::default()
        };
        viso_dsl::frontend::compile_file_for(
            source,
            &origin(),
            viso_behavior::native::Natives::standard(),
            profile,
        )
        .errors()
        .map(|d| (d.code.to_string(), d.message.clone()))
        .collect()
    }
}

impl Drop for Assets {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[test]
fn a_texture_parameter_takes_an_image_asset() {
    let assets = Assets::new("texture");
    let photo = |value: &str| painting(PHOTO, &format!("Photo {{ fade: 0.5, photo: {value} }}"));
    assert_eq!(assets.errors(&photo("asset(\"photo.png\")")), []);
    for wrong in ["asset(\"icon.svg\")", "7"] {
        let found = assets.errors(&photo(wrong));
        assert_eq!(
            found.iter().map(|(c, _)| c.as_str()).collect::<Vec<_>>(),
            ["E2103"],
            "{wrong}: {found:?}"
        );
    }
    let missing = assets.errors(&painting(PHOTO, "Photo { fade: 0.5 }"));
    assert!(
        missing.len() == 1 && missing[0].1.contains("photo"),
        "{missing:?}"
    );
}

#[test]
fn a_shader_reads_the_builtins_and_declares_none() {
    for (member, default_vertex) in [
        ("uniform viewport_size: Vec2F32", true),
        ("uniform time: F32", false),
        ("instance rect_pos: Vec2F32", true),
        ("instance rect_size: Vec2F32", false),
        ("varying uv: Vec2F32", true),
        ("varying local: Vec2F32", true),
    ] {
        let vertex = if default_vertex {
            ""
        } else {
            "vertex(vertex_id: U32) -> VertexOutput {\n\
             return VertexOutput { clip_position: to_clip(rect_pos, viewport_size) }\n}\n"
        };
        let source = format!(
            "shader Glow {{\n{member}\n{vertex}fragment() -> Vec4F32 {{ return Vec4F32(1.0) }}\n}}\n\
             export component Empty {{ view {{ Text {{ }} }} }}"
        );
        let found = errors(&source);
        assert_eq!(found.len(), 1, "{member}: {found:?}");
        assert!(
            found[0].0 == "E8107" && found[0].1.contains("builtin"),
            "{found:?}"
        );
    }
    // A hand-written vertex writes varyings of its own, `uv` among them.
    let own = "shader Glow {\nvarying uv: Vec2F32\n\
               vertex(vertex_id: U32) -> VertexOutput {\n\
               uv = quad_vertex(vertex_id)\n\
               return VertexOutput { clip_position: to_clip(rect_pos + uv * rect_size, viewport_size) }\n}\n\
               fragment() -> Vec4F32 { return Vec4F32(uv.x, uv.y, time, 1.0) }\n}\n\
               export component Empty { view { Text { } } }";
    assert_eq!(codes(own), Vec::<String>::new());
}

/// The compiled program of the one shader in `source`.
fn program_of(source: &str) -> viso_shader::program::Program {
    let compiled = compile_file(source, &origin());
    assert!(!compiled.has_errors(), "{:?}", compiled.diagnostics);
    compiled.shaders[0].program.clone().expect("a program")
}

#[test]
fn the_default_vertex_is_the_hand_written_one() {
    use viso_shader::program::{Target, emit};
    let fragment = "fragment() -> Vec4F32 { return Vec4F32(uv.x, local.y, 0.0, 1.0) }";
    let default = program_of(&format!("shader S {{\n{fragment}\n}}\n"));
    let written = program_of(&format!(
        "shader S {{
    varying uv: Vec2F32
    varying local: Vec2F32
    vertex(vertex_id: U32) -> VertexOutput {{
        let unit = quad_vertex(vertex_id)
        uv = unit
        local = unit * rect_size
        return VertexOutput {{ clip_position: to_clip(rect_pos + unit * rect_size, viewport_size) }}
    }}
    {fragment}
}}
"
    ));
    for target in [Target::Msl, Target::Wgsl, Target::Hlsl, Target::Glsl] {
        assert_eq!(
            emit(&default, &default.interface(), target),
            emit(&written, &written.interface(), target),
            "{target:?}"
        );
    }
    let unspanned = |p: &viso_shader::program::Program| {
        let text = format!("{p:?}");
        let mut out = String::new();
        let mut rest = text.as_str();
        while let Some(at) = rest.find("span: Span {") {
            out.push_str(&rest[..at]);
            rest = &rest[at + rest[at..].find('}').unwrap() + 1..];
        }
        out + rest
    };
    assert_eq!(
        unspanned(&default),
        unspanned(&written),
        "the program the interpreter runs"
    );
}

#[test]
fn a_builtin_no_body_reads_takes_no_slot() {
    let names =
        |p: &viso_shader::program::Program,
         of: fn(&viso_shader::program::Program) -> &Vec<viso_shader::program::Binding>| {
            of(p).iter().map(|b| b.name.to_string()).collect::<Vec<_>>()
        };
    let plain = program_of("shader S { fragment() -> Vec4F32 { return Vec4F32(1.0) } }");
    assert_eq!(names(&plain, |p| &p.uniforms), ["viewport_size"]);
    assert_eq!(names(&plain, |p| &p.instance), ["rect_pos", "rect_size"]);
    assert!(plain.varyings.is_empty());
    assert!(plain.vertex.is_some(), "the default vertex");

    let clocked = program_of(
        "shader S { instance k: F32\n fragment() -> Vec4F32 { return Vec4F32(local.x * k, time, 0.0, 1.0) } }",
    );
    assert_eq!(names(&clocked, |p| &p.uniforms), ["viewport_size", "time"]);
    assert_eq!(
        names(&clocked, |p| &p.instance),
        ["rect_pos", "rect_size", "k"]
    );
    assert_eq!(names(&clocked, |p| &p.varyings), ["local"]);
    let compiled = compile_file(
        &painting(
            "shader Clock { instance k: F32\n fragment() -> Vec4F32 { return Vec4F32(local.x * k, time, 0.0, 1.0) } }",
            "Clock { k: 1.0 }",
        ),
        &origin(),
    );
    assert!(!compiled.has_errors(), "{:?}", compiled.diagnostics);
    let layout = compiled.shaders[0].brush.as_ref().expect("a brush");
    assert_eq!((layout.rect_pos, layout.rect_size), (0, 8), "the box first");
    assert_eq!(layout.params[0].offset, 16);
    assert!(layout.time.is_some());
}
