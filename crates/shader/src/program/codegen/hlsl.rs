//! The HLSL interface: a `cbuffer` with a `packoffset` per leaf, a
//! `StructuredBuffer` of scalar leaves, and constructor functions for the
//! records HLSL cannot build in an expression.

use super::{Gen, LANES, entry_name, leaf_lanes, put};
use crate::program::layout::Interpolation;
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
        Ty::Texture(_) => "Texture2D<float4>".into(),
        Ty::Sampler => "SamplerState".into(),
    }
}

const HELPERS: &str = "\
float2 viso_quad_vertex(uint v) {
    switch (v % 6u) {
        case 0u: return float2(0.0f, 0.0f);
        case 1u: return float2(1.0f, 0.0f);
        case 2u: return float2(0.0f, 1.0f);
        case 3u: return float2(1.0f, 0.0f);
        case 4u: return float2(1.0f, 1.0f);
        default: return float2(0.0f, 1.0f);
    }
}

float4 viso_to_clip(float2 p, float2 viewport) {
    return float4(p.x / viewport.x * 2.0f - 1.0f, 1.0f - p.y / viewport.y * 2.0f, 0.0f, 1.0f);
}

float viso_rounded_rect_sdf(float2 p, float2 size, float radius) {
    float2 extent = size * 0.5f;
    float r = clamp(radius, 0.0f, min(extent.x, extent.y));
    float2 q = abs(p - extent) - (extent - ((float2)r));
    return length(max(q, ((float2)0.0f))) + min(max(q.x, q.y), 0.0f) - r;
}

";

/// The `packoffset` of byte `offset`.
fn pack(offset: u32) -> String {
    format!("c{}.{}", offset / 16, LANES[(offset % 16 / 4) as usize])
}

/// A struct and the function that builds it from its fields.
fn record(out: &mut String, name: &str, fields: &[(String, Ty)]) {
    put(out, format_args!("struct {name} {{\n"));
    for (field, t) in fields {
        put(out, format_args!("    {} {field};\n", ty(*t)));
    }
    let params: Vec<String> = fields
        .iter()
        .enumerate()
        .map(|(j, (_, t))| format!("{} a{j}", ty(*t)))
        .collect();
    put(
        out,
        format_args!(
            "}};\n\n{name} make_{name}({}) {{\n    {name} r;\n",
            params.join(", ")
        ),
    );
    for (j, (field, _)) in fields.iter().enumerate() {
        put(out, format_args!("    r.{field} = a{j};\n"));
    }
    out.push_str("    return r;\n}\n\n");
}

pub(super) fn module(g: &mut Gen<'_>) {
    g.out.push_str(HELPERS);
    for (index, r) in g.p.records.iter().enumerate() {
        let fields: Vec<(String, Ty)> = r
            .fields
            .iter()
            .enumerate()
            .map(|(f, b)| (format!("f{f}"), b.ty))
            .collect();
        record(&mut g.out, &format!("R{index}"), &fields);
    }
    record(
        &mut g.out,
        "VertexOutput",
        &[("clip_position".to_owned(), Ty::VEC4)],
    );

    if !g.i.uniforms.fields.is_empty() {
        g.out.push_str("cbuffer Uniforms : register(b0) {\n");
        for (k, f) in g.i.uniforms.fields.iter().enumerate() {
            match f.ty {
                Ty::Matrix(n) => {
                    for c in 0..n {
                        let at = pack(f.offset + u32::from(c) * f.matrix_stride);
                        put(
                            &mut g.out,
                            format_args!("    float{n} u{k}_{c} : packoffset({at});\n"),
                        );
                    }
                }
                t => put(
                    &mut g.out,
                    format_args!("    {} u{k} : packoffset({});\n", ty(t), pack(f.offset)),
                ),
            }
        }
        g.out.push_str("};\n\n");
    }
    if !g.i.instance.fields.is_empty() {
        g.out.push_str("struct Instance {\n");
        for (k, f) in g.i.instance.fields.iter().enumerate() {
            let lane = match f.ty.scalar() {
                Some(Scalar::I32) => "int",
                Some(Scalar::U32) => "uint",
                _ => "float",
            };
            for j in 0..leaf_lanes(f.ty) {
                put(&mut g.out, format_args!("    {lane} i{k}_{j};\n"));
            }
        }
        g.out
            .push_str("};\n\nStructuredBuffer<Instance> instances : register(t0, space1);\n\n");
    }
    for t in &g.i.textures {
        put(
            &mut g.out,
            format_args!("Texture2D<float4> t{0} : register(t{0});\n", t.index),
        );
    }
    for s in &g.i.samplers {
        put(
            &mut g.out,
            format_args!("SamplerState s{0} : register(s{0});\n", s.index),
        );
    }
    if !g.i.textures.is_empty() || !g.i.samplers.is_empty() {
        g.out.push('\n');
    }

    g.out
        .push_str("struct VOut {\n    float4 clip_position : SV_Position;\n");
    let mut location = 0;
    for v in &g.i.varyings {
        let flat = if v.interpolation == Interpolation::Flat {
            "nointerpolation "
        } else {
            ""
        };
        put(
            &mut g.out,
            format_args!(
                "    {flat}{} v{} : TEXCOORD{location};\n",
                ty(v.ty),
                v.offset
            ),
        );
        location += 1;
    }
    for (k, f) in g.i.instance.fields.iter().enumerate() {
        if !g.forwarded.contains(&f.member) {
            continue;
        }
        let mut field = |name: String, t: Ty| {
            put(
                &mut g.out,
                format_args!(
                    "    nointerpolation {} {name} : TEXCOORD{location};\n",
                    ty(t)
                ),
            );
            location += 1;
        };
        match f.ty {
            Ty::Matrix(n) => {
                (0..n).for_each(|c| field(format!("fi{k}_{c}"), Ty::Vector(Scalar::F32, n)))
            }
            t => field(format!("fi{k}"), t),
        }
    }
    g.out.push_str("};\n\n");

    g.functions();
    let p = g.p;
    if let Some(f) = &p.vertex {
        put(
            &mut g.out,
            format_args!(
                "VOut {}(uint vid : SV_VertexID, uint iid : SV_InstanceID) {{\n",
                entry_name(Stage::Vertex)
            ),
        );
        g.entry_body(f);
    }
    if let Some(f) = &p.fragment {
        put(
            &mut g.out,
            format_args!(
                "float4 {}(VOut vin) : SV_Target {{\n",
                entry_name(Stage::Fragment)
            ),
        );
        g.entry_body(f);
    }
}
