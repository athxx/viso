//! IR → GLSL ES 3.00 codegen, for the GLES3 backend.
//!
//! [`emit_glsl`] prints one source holding both stages, each under its
//! [`VERTEX_DEFINE`] / fragment branch; the backend compiles it twice behind
//! the [`stage_prelude`] of each stage. GLSL ES 3.00 has no interface blocks
//! between stages, so the bodies run as plain functions over the IR's own
//! structs and each stage's `main` moves them across the interface:
//!
//! - attribute fields are `layout(location = i) in`, in declaration order;
//!   the vertex `main` gathers them into the attribute struct the body's
//!   `instances[iid]` / `verts[vid]` fetch reads, with `gl_VertexID` and
//!   `gl_InstanceID` as `vid` / `iid`;
//! - `VOut`'s position is `gl_Position`; every other varying is one
//!   `out`/`in` variable, `flat` for integer and `[[flat]]` varyings;
//! - the uniforms are a `std140` block (`ViewBlock`, binding 0), whose layout
//!   is the inline bytes' own: every member is a 4-byte scalar or a vector of
//!   two or four;
//! - `tex` / `dst_tex` are combined samplers on texture units 0 and 1; the
//!   bind group's sampler state rides a sampler object bound to both units;
//! - `viso_target` is `(y sign, framebuffer height, quarter turns,
//!   framebuffer width)`: a texture target renders with clip-space y negated,
//!   so its rows are stored top-down like every other backend's; a surface
//!   whose panel the display cannot turn is drawn turned (`viso_place`) onto
//!   a framebuffer the panel's shape; and the fragment's position is rebuilt
//!   top-left in the image from `gl_FragCoord` for every kind of target
//!   (`viso_frag_position`).

use std::fmt::Write as _;

use crate::ir::body::print::{ATTRS_PARAM, INSTANCE_PARAM, Lang, Printer};
use crate::ir::body::{BodyError, parse_ir};
use crate::ir::module::{ShaderIr, VertexSource};
use crate::ir::types::{IrType, ScalarType};

/// The macro the vertex stage's prelude defines.
pub const VERTEX_DEFINE: &str = "VISO_VERTEX";

/// The uniform block every built-in reads, at binding 0.
pub const UNIFORM_BLOCK: &str = "ViewBlock";

/// The uniform carrying `(y sign, framebuffer height, quarter turns,
/// framebuffer width)`.
pub const TARGET_UNIFORM: &str = "viso_target";

/// The target uniform and the stage's function that places the image on the
/// framebuffer: `viso_place` takes a clip position there (y flipped for a
/// texture, turned counter-clockwise by the quarter turns), and
/// `viso_frag_position` takes a fragment back to its top-left position in
/// the image.
pub const TARGET_GLSL: &str = "uniform vec4 viso_target;
#ifdef VISO_VERTEX
vec4 viso_place(vec4 p) {
    p.y *= viso_target.x;
    int t = int(viso_target.z);
    p.xy = t == 1 ? vec2(-p.y, p.x) : t == 2 ? -p.xy : t == 3 ? vec2(p.y, -p.x) : p.xy;
    return p;
}
#else
vec4 viso_frag_position() {
    vec2 f = vec2(gl_FragCoord.x, viso_target.x > 0.0 ? viso_target.y - gl_FragCoord.y : gl_FragCoord.y);
    int t = int(viso_target.z);
    f = t == 1 ? vec2(viso_target.y - f.y, f.x)
        : t == 2 ? vec2(viso_target.w - f.x, viso_target.y - f.y)
        : t == 3 ? vec2(f.y, viso_target.w - f.x)
        : f;
    return vec4(f, gl_FragCoord.zw);
}
#endif
";

/// The lines a stage's source opens with: the version, and for the vertex
/// stage the [`VERTEX_DEFINE`] that selects its half.
pub fn stage_prelude(vertex: bool) -> &'static str {
    if vertex {
        "#version 300 es\n#define VISO_VERTEX 1\n"
    } else {
        "#version 300 es\n"
    }
}

/// Emit the GLSL ES 3.00 program for `ir`, both stages, without its
/// [`stage_prelude`].
pub fn emit_glsl(ir: &ShaderIr) -> Result<String, BodyError> {
    let parsed = parse_ir(ir)?;
    let names = Printer::new(Lang::Glsl);
    let mut out = String::with_capacity(4096);
    out.push_str("precision highp float;\nprecision highp int;\n\n");

    let attrs = ir.vertex_source.attr_struct_name();
    let _ = writeln!(out, "struct {attrs} {{");
    for f in ir.attributes {
        let _ = writeln!(out, "    {} {};", glsl_type(f.ty), names.ident(f.name));
    }
    out.push_str("};\n\n");
    let instanced = ir.vertex_source == VertexSource::PerVertexInstanced;
    if instanced {
        out.push_str("struct InstanceIn {\n");
        for f in ir.instance_attributes {
            let _ = writeln!(out, "    {} {};", glsl_type(f.ty), names.ident(f.name));
        }
        out.push_str("};\n\n");
    }

    out.push_str("struct Uniforms {\n");
    for f in ir.uniforms {
        let _ = writeln!(out, "    {} {};", glsl_type(f.ty), names.ident(f.name));
    }
    let _ = writeln!(
        out,
        "}};\n\nlayout(std140) uniform {UNIFORM_BLOCK} {{\n    Uniforms uniforms;\n}};\n"
    );

    out.push_str("struct VOut {\n");
    for v in ir.varyings {
        let _ = writeln!(out, "    {} {};", glsl_type(v.ty), names.ident(v.name));
    }
    out.push_str("};\n\n");

    out.push_str(TARGET_GLSL);
    if ir.texture_count >= 1 {
        out.push_str("uniform highp sampler2D tex;\n");
        if ir.texture_count >= 2 {
            out.push_str("uniform highp sampler2D dst_tex;\n");
        }
    }
    out.push('\n');

    // The varyings besides the position, with their interface names.
    let varyings: Vec<(String, &crate::ir::module::Varying)> = ir
        .varyings
        .iter()
        .filter(|v| !v.attr.contains("[[position]]"))
        .enumerate()
        .map(|(i, v)| (format!("viso_v{i}"), v))
        .collect();
    let position = ir
        .varyings
        .iter()
        .find(|v| v.attr.contains("[[position]]"))
        .map(|v| names.ident(v.name).into_owned());
    let qualifier = |v: &crate::ir::module::Varying| {
        if v.attr.contains("[[flat]]") || !is_float(v.ty) {
            "flat "
        } else {
            ""
        }
    };

    let _ = writeln!(out, "#ifdef {VERTEX_DEFINE}\n");
    let inputs: Vec<_> = ir
        .attributes
        .iter()
        .chain(if instanced {
            ir.instance_attributes
        } else {
            &[]
        })
        .collect();
    for (i, f) in inputs.iter().enumerate() {
        let _ = writeln!(
            out,
            "layout(location = {i}) in {} viso_a{i};",
            glsl_type(f.ty)
        );
    }
    for (name, v) in &varyings {
        let _ = writeln!(out, "{}out {} {name};", qualifier(v), glsl_type(v.ty));
    }
    let instance = if instanced {
        format!(", InstanceIn {INSTANCE_PARAM}")
    } else {
        String::new()
    };
    let _ = writeln!(
        out,
        "\nVOut vertex_main(uint vid, uint iid, {attrs} {ATTRS_PARAM}{instance}) {{"
    );
    let mut p = Printer::new(Lang::Glsl)
        .with_source(ir.vertex_source)
        .with_depth(1);
    p.entry_body(&parsed.vertex);
    out.push_str(&p.finish());
    out.push_str("}\n\nvoid main() {\n");
    let _ = writeln!(out, "    {attrs} a;");
    for (i, f) in ir.attributes.iter().enumerate() {
        let _ = writeln!(out, "    a.{} = viso_a{i};", names.ident(f.name));
    }
    if instanced {
        out.push_str("    InstanceIn b;\n");
        for (i, f) in ir.instance_attributes.iter().enumerate() {
            let _ = writeln!(
                out,
                "    b.{} = viso_a{};",
                names.ident(f.name),
                ir.attributes.len() + i
            );
        }
        out.push_str("    VOut o = vertex_main(uint(gl_VertexID), uint(gl_InstanceID), a, b);\n");
    } else {
        out.push_str("    VOut o = vertex_main(uint(gl_VertexID), uint(gl_InstanceID), a);\n");
    }
    if let Some(position) = &position {
        let _ = writeln!(out, "    gl_Position = viso_place(o.{position});");
    }
    for (name, v) in &varyings {
        let _ = writeln!(out, "    {name} = o.{};", names.ident(v.name));
    }
    out.push_str("}\n\n#else\n\n");

    for (name, v) in &varyings {
        let _ = writeln!(out, "{}in {} {name};", qualifier(v), glsl_type(v.ty));
    }
    out.push_str("layout(location = 0) out vec4 viso_color;\n");
    if !parsed.helpers.is_empty() {
        out.push('\n');
        let mut p = Printer::new(Lang::Glsl);
        p.items(&parsed.helpers);
        out.push_str(&p.finish());
    }
    out.push_str("\nvec4 fragment_main(VOut vin) {\n");
    let mut p = Printer::new(Lang::Glsl)
        .with_signatures(&parsed.helpers)
        .with_depth(1);
    p.entry_body(&parsed.fragment);
    out.push_str(&p.finish());
    out.push_str("}\n\nvoid main() {\n    VOut vin;\n");
    if let Some(position) = &position {
        let _ = writeln!(out, "    vin.{position} = viso_frag_position();");
    }
    for (name, v) in &varyings {
        let _ = writeln!(out, "    vin.{} = {name};", names.ident(v.name));
    }
    out.push_str("    viso_color = fragment_main(vin);\n}\n\n#endif\n");
    Ok(out)
}

/// The GLSL spelling of an interface type.
pub(crate) fn glsl_type(t: IrType) -> String {
    match t {
        IrType::Scalar(ScalarType::F32) => "float".into(),
        IrType::Scalar(ScalarType::U32) => "uint".into(),
        IrType::Vector {
            scalar: ScalarType::F32,
            lanes,
        } => format!("vec{lanes}"),
        IrType::Vector {
            scalar: ScalarType::U32,
            lanes,
        } => format!("uvec{lanes}"),
    }
}

fn is_float(t: IrType) -> bool {
    matches!(
        t,
        IrType::Scalar(ScalarType::F32)
            | IrType::Vector {
                scalar: ScalarType::F32,
                ..
            }
    )
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::ir::module::builtin_irs;
    use std::path::PathBuf;
    use std::process::Command;

    /// Compiles one stage of `src` with glslang as GLSL ES 3.00; `None` when
    /// glslang is not installed.
    pub(crate) fn glslang(src: &str, vertex: bool, tag: &str) -> Option<Result<(), String>> {
        let dir: PathBuf = std::env::temp_dir().join("viso_glsl_check");
        std::fs::create_dir_all(&dir).ok()?;
        let ext = if vertex { "vert" } else { "frag" };
        let path = dir.join(format!("{tag}.{ext}"));
        std::fs::write(&path, format!("{}{src}", stage_prelude(vertex))).ok()?;
        let out = Command::new("glslangValidator").arg(&path).output().ok()?;
        if !out.status.success() {
            return Some(Err(String::from_utf8_lossy(&out.stdout).into_owned()));
        }
        Some(Ok(()))
    }

    #[test]
    fn every_builtin_compiles_as_glsl_es_300() {
        for ir in builtin_irs() {
            let src = emit_glsl(&ir).unwrap();
            let tag = format!("{:?}", ir.kind);
            for vertex in [true, false] {
                match glslang(&src, vertex, &tag) {
                    None => {
                        eprintln!("glslangValidator not installed; GLSL unchecked");
                        return;
                    }
                    Some(Err(e)) => panic!("{tag} vertex={vertex}:\n{e}\n{src}"),
                    Some(Ok(())) => {}
                }
            }
        }
    }

    #[test]
    fn attributes_follow_declaration_order() {
        let ir = builtin_irs()[0].clone();
        let src = emit_glsl(&ir).unwrap();
        for (i, f) in ir.attributes.iter().enumerate() {
            assert!(
                src.contains(&format!(
                    "layout(location = {i}) in {} viso_a{i};",
                    glsl_type(f.ty)
                )),
                "{src}"
            );
        }
        assert!(src.contains("layout(std140) uniform ViewBlock"));
        assert!(
            src.contains("gl_Position = viso_place(o.position);"),
            "{src}"
        );
    }
}
