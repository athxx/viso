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
    uniform scale: F32;
    instance rect_pos: Vec2F32;
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
    assert_eq!(found_codes, ["E8107"; 4], "{found:#?}");
    let messages: Vec<&str> = found.iter().map(|d| d.message.as_str()).collect();
    for part in [
        "`scale`",
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

/// A shader using every construct a program holds: a record, a `fn`, a
/// varying, the fragment's builtin, a loop, branches, swizzles, an index, a
/// conversion and a discard.
const EVERY: &str = "@shader_value
record Light { position: Vec2F32; strength: F32; }

shader Every {
    uniform viewport_size: Vec2F32;
    uniform light: Light;
    uniform mode: I32;
    instance rect_pos: Vec2F32;
    instance rect_size: Vec2F32;
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
    uniform viewport_size: Vec2F32;
    instance rect_pos: Vec2F32;
    instance rect_size: Vec2F32;
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
