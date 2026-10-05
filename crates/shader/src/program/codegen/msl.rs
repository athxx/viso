//! The Metal Shading Language interface: packed buffer structs whose sizes
//! the compiler asserts, and entries taking the buffers as arguments.

use super::{Gen, entry_name, put};
use crate::program::layout::{BlockLayout, Interpolation};
use crate::program::{Scalar, Stage, Ty};

pub(super) fn ty(ty: Ty) -> String {
    let scalar = |s: Scalar| match s {
        Scalar::Bool => "bool",
        Scalar::I32 => "int",
        Scalar::U32 => "uint",
        Scalar::F32 => "float",
    };
    match ty {
        Ty::Scalar(s) => scalar(s).into(),
        Ty::Vector(s, n) => format!("{}{n}", scalar(s)),
        Ty::Matrix(n) => format!("float{n}x{n}"),
        Ty::Color => "float4".into(),
        Ty::Record(i) => format!("R{i}"),
        Ty::VertexOutput => "VertexOutput".into(),
        Ty::Unit => "void".into(),
        Ty::Texture(_) => "texture2d<float>".into(),
        Ty::Sampler => "sampler".into(),
    }
}

/// A leaf as a buffer stores it: packed vectors, natural matrices.
fn storage(t: Ty) -> String {
    match t {
        Ty::Vector(..) | Ty::Color => format!("packed_{}", ty(t)),
        other => ty(other),
    }
}

const HELPERS: &str = "\
static inline float2 viso_quad_vertex(uint v) {
    switch (v % 6u) {
        case 0u: return float2(0.0f, 0.0f);
        case 1u: return float2(1.0f, 0.0f);
        case 2u: return float2(0.0f, 1.0f);
        case 3u: return float2(1.0f, 0.0f);
        case 4u: return float2(1.0f, 1.0f);
        default: return float2(0.0f, 1.0f);
    }
}

static inline float4 viso_to_clip(float2 p, float2 viewport) {
    return float4(p.x / viewport.x * 2.0f - 1.0f, 1.0f - p.y / viewport.y * 2.0f, 0.0f, 1.0f);
}

static inline float viso_rounded_rect_sdf(float2 p, float2 size, float radius) {
    float2 extent = size * 0.5f;
    float r = clamp(radius, 0.0f, min(extent.x, extent.y));
    float2 q = abs(p - extent) - (extent - float2(r));
    return length(max(q, float2(0.0f))) + min(max(q.x, q.y), 0.0f) - r;
}

";

pub(super) fn module(g: &mut Gen<'_>) {
    g.out
        .push_str("#include <metal_stdlib>\nusing namespace metal;\n\n");
    g.out.push_str(HELPERS);
    for (index, record) in g.p.records.iter().enumerate() {
        put(&mut g.out, format_args!("struct R{index} {{\n"));
        for (f, field) in record.fields.iter().enumerate() {
            put(&mut g.out, format_args!("    {} f{f};\n", ty(field.ty)));
        }
        g.out.push_str("};\n\n");
    }
    g.out
        .push_str("struct VertexOutput {\n    float4 clip_position;\n};\n\n");
    block(&mut g.out, "Uniforms", 'u', &g.i.uniforms, false);
    block(&mut g.out, "Instance", 'i', &g.i.instance, true);
    vout(g);
    g.functions();
    for stage in [Stage::Vertex, Stage::Fragment] {
        let f = match stage {
            Stage::Vertex => g.p.vertex.as_ref(),
            Stage::Fragment => g.p.fragment.as_ref(),
        };
        let Some(f) = f else { continue };
        let mut params = Vec::new();
        let head = match stage {
            Stage::Vertex => {
                params.push("uint vid [[vertex_id]]".to_owned());
                params.push("uint iid [[instance_id]]".to_owned());
                params.extend(bindings(g, g.i.vertex_textures));
                if !g.i.instance.fields.is_empty() {
                    params.push("device const Instance* instances [[buffer(1)]]".to_owned());
                }
                "vertex VOut"
            }
            Stage::Fragment => {
                params.push("VOut vin [[stage_in]]".to_owned());
                params.extend(bindings(g, true));
                "fragment float4"
            }
        };
        put(
            &mut g.out,
            format_args!("{head} {}({}) {{\n", entry_name(stage), params.join(", ")),
        );
        g.entry_body(f);
    }
}

/// A block's struct, each leaf at its offset after explicit padding; the
/// compiler checks the total.
fn block(out: &mut String, name: &str, prefix: char, block: &BlockLayout, columns: bool) {
    if block.fields.is_empty() {
        return;
    }
    put(out, format_args!("struct {name} {{\n"));
    let mut cursor = 0;
    for (k, f) in block.fields.iter().enumerate() {
        if f.offset > cursor {
            put(
                out,
                format_args!("    char pad{k}[{}];\n", f.offset - cursor),
            );
        }
        match f.ty {
            Ty::Matrix(n) if columns => {
                for c in 0..n {
                    put(out, format_args!("    packed_float{n} {prefix}{k}_{c};\n"));
                }
            }
            t => put(out, format_args!("    {} {prefix}{k};\n", storage(t))),
        }
        cursor = f.offset + f.size;
    }
    if block.size > cursor {
        put(
            out,
            format_args!("    char pad_end[{}];\n", block.size - cursor),
        );
    }
    put(
        out,
        format_args!(
            "}};\nstatic_assert(sizeof({name}) == {0}, \"{name} is laid out in {0} bytes\");\n\n",
            block.size
        ),
    );
}

fn vout(g: &mut Gen<'_>) {
    g.out
        .push_str("struct VOut {\n    float4 clip_position [[position]];\n");
    for v in &g.i.varyings {
        let flat = if v.interpolation == Interpolation::Flat {
            " [[flat]]"
        } else {
            ""
        };
        put(
            &mut g.out,
            format_args!("    {} v{}{flat};\n", ty(v.ty), v.offset),
        );
    }
    for (k, f) in g.i.instance.fields.iter().enumerate() {
        if !g.forwarded.contains(&f.member) {
            continue;
        }
        match f.ty {
            Ty::Matrix(n) => {
                for c in 0..n {
                    put(
                        &mut g.out,
                        format_args!("    float{n} fi{k}_{c} [[flat]];\n"),
                    );
                }
            }
            t => put(&mut g.out, format_args!("    {} fi{k} [[flat]];\n", ty(t))),
        }
    }
    g.out.push_str("};\n\n");
}

fn bindings(g: &Gen<'_>, textures: bool) -> Vec<String> {
    let mut params = Vec::new();
    if !g.i.uniforms.fields.is_empty() {
        params.push("constant Uniforms& u [[buffer(0)]]".to_owned());
    }
    if textures {
        for t in &g.i.textures {
            params.push(format!("texture2d<float> t{0} [[texture({0})]]", t.index));
        }
        for s in &g.i.samplers {
            params.push(format!("sampler s{0} [[sampler({0})]]", s.index));
        }
    }
    params
}
