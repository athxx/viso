//! The GLSL ES 3.00 interface. GLSL ES 3.00 has neither storage buffers nor
//! explicit block offsets, so both blocks are read as raw words:
//!
//! - the uniform block is `std140` `uvec4` words at binding 0, each leaf
//!   rebuilt with `uintBitsToFloat` from its byte offset in the layout;
//! - the instances are `std140` `uvec4` words at binding 1 holding
//!   [`INSTANCE_BLOCK_BYTES`] at most: the backend binds a draw's instances a
//!   chunk at a time and sets `viso_base_instance` to the chunk's first, so
//!   the program sees the draw's own instance index;
//! - textures are combined samplers `t<i>` on texture unit `i`; the sampler
//!   state rides a sampler object on the unit, so a sampler value is a
//!   placeholder `int` constant.
//!
//! Both stages share one source, the vertex half under `VISO_VERTEX`
//! ([`crate::ir::codegen_glsl::stage_prelude`]); `VOut` crosses the stage
//! interface member by member, and `viso_target` places the image on the
//! framebuffer as the built-ins do ([`crate::ir::codegen_glsl::TARGET_GLSL`]).

use super::{Gen, entry_name, leaf_lanes, put};
use crate::program::layout::Interpolation;
use crate::program::{Scalar, Stage, Ty};

/// The bytes one bound chunk of instances may hold: the uniform block size
/// every GLES 3.0 device supports.
pub const INSTANCE_BLOCK_BYTES: u32 = 16384;

pub(super) fn ty(ty: Ty) -> String {
    let prefix = |s: Scalar| match s {
        Scalar::Bool => "b",
        Scalar::I32 => "i",
        Scalar::U32 => "u",
        Scalar::F32 => "",
    };
    match ty {
        Ty::Scalar(Scalar::Bool) => "bool".into(),
        Ty::Scalar(Scalar::I32) => "int".into(),
        Ty::Scalar(Scalar::U32) => "uint".into(),
        Ty::Scalar(Scalar::F32) => "float".into(),
        Ty::Vector(s, n) => format!("{}vec{n}", prefix(s)),
        Ty::Matrix(n) => format!("mat{n}"),
        Ty::Color => "vec4".into(),
        Ty::Record(i) => format!("R{i}"),
        Ty::VertexOutput => "VertexOutput".into(),
        Ty::Unit => "void".into(),
        Ty::Texture(_) => "highp sampler2D".into(),
        Ty::Sampler => "int".into(),
    }
}

const HELPERS: &str = "\
precision highp float;
precision highp int;

vec2 viso_quad_vertex(uint v) {
    switch (v % 6u) {
        case 0u: return vec2(0.0f, 0.0f);
        case 1u: return vec2(1.0f, 0.0f);
        case 2u: return vec2(0.0f, 1.0f);
        case 3u: return vec2(1.0f, 0.0f);
        case 4u: return vec2(1.0f, 1.0f);
        default: return vec2(0.0f, 1.0f);
    }
}

vec4 viso_to_clip(vec2 p, vec2 viewport) {
    return vec4(p.x / viewport.x * 2.0f - 1.0f, 1.0f - p.y / viewport.y * 2.0f, 0.0f, 1.0f);
}

float viso_rounded_rect_sdf(vec2 p, vec2 size, float radius) {
    vec2 extent = size * 0.5f;
    float r = clamp(radius, 0.0f, min(extent.x, extent.y));
    vec2 q = abs(p - extent) - (extent - vec2(r));
    return length(max(q, vec2(0.0f))) + min(max(q.x, q.y), 0.0f) - r;
}

float viso_fmod(float a, float b) { return a - b * trunc(a / b); }
vec2 viso_fmod(vec2 a, vec2 b) { return a - b * trunc(a / b); }
vec3 viso_fmod(vec3 a, vec3 b) { return a - b * trunc(a / b); }
vec4 viso_fmod(vec4 a, vec4 b) { return a - b * trunc(a / b); }

";

/// The word `w` of a `uvec4` word array.
fn word(array: &str, w: &str) -> String {
    format!("{array}[({w}) / 4u][({w}) % 4u]")
}

/// A leaf lane read from word `w`, as a value of lane scalar `s`.
fn lane(s: Option<Scalar>, raw: String) -> String {
    match s {
        Some(Scalar::I32) => format!("int({raw})"),
        Some(Scalar::U32) => raw,
        Some(Scalar::Bool) => format!("({raw} != 0u)"),
        _ => format!("uintBitsToFloat({raw})"),
    }
}

/// The lanes of a leaf of `t` from word `w`: a vector's together, a
/// matrix's column by column `stride` bytes apart.
fn leaf_from(array: &str, t: Ty, offset: u32, matrix_stride: u32) -> String {
    let at = |o: u32| lane(t.scalar(), word(array, &format!("{}u", o / 4)));
    match t {
        Ty::Matrix(n) => {
            let columns: Vec<String> = (0..u32::from(n))
                .map(|c| {
                    let base = offset + c * matrix_stride;
                    let lanes: Vec<String> = (0..u32::from(n)).map(|j| at(base + j * 4)).collect();
                    format!("vec{n}({})", lanes.join(", "))
                })
                .collect();
            format!("mat{n}({})", columns.join(", "))
        }
        _ => {
            let n = u32::from(leaf_lanes(t));
            if n == 1 {
                at(offset)
            } else {
                let lanes: Vec<String> = (0..n).map(|j| at(offset + j * 4)).collect();
                format!("{}({})", ty(t), lanes.join(", "))
            }
        }
    }
}

pub(super) fn module(g: &mut Gen<'_>) {
    g.out.push_str(HELPERS);
    g.out.push_str(crate::ir::codegen_glsl::TARGET_GLSL);
    g.out.push('\n');
    for (index, record) in g.p.records.iter().enumerate() {
        put(&mut g.out, format_args!("struct R{index} {{\n"));
        for (f, field) in record.fields.iter().enumerate() {
            put(&mut g.out, format_args!("    {} f{f};\n", ty(field.ty)));
        }
        g.out.push_str("};\n\n");
    }
    g.out
        .push_str("struct VertexOutput {\n    vec4 clip_position;\n};\n\n");

    let uniforms = &g.i.uniforms;
    if !uniforms.fields.is_empty() {
        put(
            &mut g.out,
            format_args!(
                "layout(std140) uniform ProgramUniforms {{\n    uvec4 viso_u[{}];\n}};\n\n",
                uniforms.size.div_ceil(16).max(1)
            ),
        );
        for (k, f) in uniforms.fields.iter().enumerate() {
            put(
                &mut g.out,
                format_args!(
                    "{} viso_uniform{k}() {{\n    return {};\n}}\n\n",
                    ty(f.ty),
                    leaf_from("viso_u", f.ty, f.offset, f.matrix_stride)
                ),
            );
        }
    }
    let instance = &g.i.instance;
    if !instance.fields.is_empty() {
        put(
            &mut g.out,
            format_args!(
                "layout(std140) uniform ProgramInstances {{\n    uvec4 viso_i[{}];\n}};\n\
                 uniform uint viso_base_instance;\n\nstruct Instance {{\n",
                INSTANCE_BLOCK_BYTES / 16
            ),
        );
        for (k, f) in instance.fields.iter().enumerate() {
            let lane = match f.ty.scalar() {
                Some(Scalar::I32) => "int",
                Some(Scalar::U32) => "uint",
                _ => "float",
            };
            for j in 0..leaf_lanes(f.ty) {
                put(&mut g.out, format_args!("    {lane} i{k}_{j};\n"));
            }
        }
        put(
            &mut g.out,
            format_args!(
                "}};\n\nInstance viso_instance(uint i) {{\n    uint b = i * {}u;\n    Instance r;\n",
                instance.size / 4
            ),
        );
        for (k, f) in instance.fields.iter().enumerate() {
            for j in 0..u32::from(leaf_lanes(f.ty)) {
                let w = format!("b + {}u", f.offset / 4 + j);
                let value = lane(f.ty.scalar(), word("viso_i", &w));
                put(&mut g.out, format_args!("    r.i{k}_{j} = {value};\n"));
            }
        }
        g.out.push_str("    return r;\n}\n\n");
    }
    for t in &g.i.textures {
        put(
            &mut g.out,
            format_args!("uniform highp sampler2D t{};\n", t.index),
        );
    }
    for s in &g.i.samplers {
        put(&mut g.out, format_args!("const int s{0} = {0};\n", s.index));
    }
    if !g.i.textures.is_empty() || !g.i.samplers.is_empty() {
        g.out.push('\n');
    }

    // `VOut` and the interface variables each member crosses as.
    let mut members: Vec<(String, Ty, bool)> = Vec::new();
    for v in &g.i.varyings {
        let flat = v.interpolation == Interpolation::Flat || !v.ty.is_float();
        members.push((format!("v{}", v.offset), v.ty, flat));
    }
    for (k, f) in g.i.instance.fields.iter().enumerate() {
        if !g.forwarded.contains(&f.member) {
            continue;
        }
        match f.ty {
            Ty::Matrix(n) => {
                for c in 0..n {
                    members.push((format!("fi{k}_{c}"), Ty::Vector(Scalar::F32, n), true));
                }
            }
            t => members.push((format!("fi{k}"), t, true)),
        }
    }
    g.out.push_str("struct VOut {\n    vec4 clip_position;\n");
    for (name, t, _) in &members {
        put(&mut g.out, format_args!("    {} {name};\n", ty(*t)));
    }
    g.out.push_str(
        "};\n\nVOut viso_vout_zero() {\n    VOut o;\n    o.clip_position = vec4(0.0f);\n",
    );
    for (name, t, _) in &members {
        let zero = g.zero(*t);
        put(&mut g.out, format_args!("    o.{name} = {zero};\n"));
    }
    g.out.push_str("    return o;\n}\n\n");

    g.functions();
    let p = g.p;
    let qualifier = |flat: bool| if flat { "flat " } else { "" };
    g.out.push_str("#ifdef VISO_VERTEX\n\n");
    for (name, t, flat) in &members {
        put(
            &mut g.out,
            format_args!("{}out {} viso_{name};\n", qualifier(*flat), ty(*t)),
        );
    }
    if let Some(f) = &p.vertex {
        put(
            &mut g.out,
            format_args!(
                "\nVOut {}(uint vid, uint iid) {{\n",
                entry_name(Stage::Vertex)
            ),
        );
        g.entry_body(f);
        let base = if g.i.instance.fields.is_empty() {
            ""
        } else {
            " + viso_base_instance"
        };
        put(
            &mut g.out,
            format_args!(
                "void main() {{\n    VOut o = {}(uint(gl_VertexID), uint(gl_InstanceID){base});\n    \
                 gl_Position = viso_place(o.clip_position);\n",
                entry_name(Stage::Vertex)
            ),
        );
        for (name, _, _) in &members {
            put(&mut g.out, format_args!("    viso_{name} = o.{name};\n"));
        }
        g.out.push_str("}\n\n");
    } else {
        g.out
            .push_str("void main() {\n    gl_Position = vec4(0.0f);\n}\n\n");
    }
    g.out.push_str("#else\n\n");
    for (name, t, flat) in &members {
        put(
            &mut g.out,
            format_args!("{}in {} viso_{name};\n", qualifier(*flat), ty(*t)),
        );
    }
    g.out
        .push_str("layout(location = 0) out vec4 viso_color;\n\n");
    if let Some(f) = &p.fragment {
        put(
            &mut g.out,
            format_args!("vec4 {}(VOut vin) {{\n", entry_name(Stage::Fragment)),
        );
        g.entry_body(f);
        g.out.push_str(
            "void main() {\n    VOut vin;\n    vin.clip_position = viso_frag_position();\n",
        );
        for (name, _, _) in &members {
            put(&mut g.out, format_args!("    vin.{name} = viso_{name};\n"));
        }
        put(
            &mut g.out,
            format_args!(
                "    viso_color = {}(vin);\n}}\n\n",
                entry_name(Stage::Fragment)
            ),
        );
    } else {
        g.out
            .push_str("void main() {\n    viso_color = vec4(0.0f);\n}\n\n");
    }
    g.out.push_str("#endif\n");
}
