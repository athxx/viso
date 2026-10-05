//! The WGSL interface: module-scope bindings, the uniform struct pinned to
//! the layout with `@size`, the instance array of scalar leaves.

use super::{Gen, entry_name, leaf_lanes, put};
use crate::program::layout::Interpolation;
use crate::program::{Scalar, Stage, Ty};

pub(super) fn ty(ty: Ty) -> String {
    let scalar = |s: Scalar| match s {
        Scalar::Bool => "bool",
        Scalar::I32 => "i32",
        Scalar::U32 => "u32",
        Scalar::F32 => "f32",
    };
    match ty {
        Ty::Scalar(s) => scalar(s).into(),
        Ty::Vector(s, n) => format!("vec{n}<{}>", scalar(s)),
        Ty::Matrix(n) => format!("mat{n}x{n}<f32>"),
        Ty::Color => "vec4<f32>".into(),
        Ty::Record(i) => format!("R{i}"),
        Ty::VertexOutput => "VertexOutput".into(),
        Ty::Unit => "void".into(),
        Ty::Texture(_) => "texture_2d<f32>".into(),
        Ty::Sampler => "sampler".into(),
    }
}

/// The scalar a leaf's lanes are.
fn lane_type(t: Ty) -> &'static str {
    match t.scalar() {
        Some(Scalar::I32) => "i32",
        Some(Scalar::U32) => "u32",
        _ => "f32",
    }
}

const HELPERS: &str = "\
fn viso_quad_vertex(v: u32) -> vec2<f32> {
    switch (v % 6u) {
        case 0u: { return vec2<f32>(0.0f, 0.0f); }
        case 1u: { return vec2<f32>(1.0f, 0.0f); }
        case 2u: { return vec2<f32>(0.0f, 1.0f); }
        case 3u: { return vec2<f32>(1.0f, 0.0f); }
        case 4u: { return vec2<f32>(1.0f, 1.0f); }
        default: { return vec2<f32>(0.0f, 1.0f); }
    }
}

fn viso_to_clip(p: vec2<f32>, viewport: vec2<f32>) -> vec4<f32> {
    return vec4<f32>(p.x / viewport.x * 2.0f - 1.0f, 1.0f - p.y / viewport.y * 2.0f, 0.0f, 1.0f);
}

fn viso_rounded_rect_sdf(p: vec2<f32>, size: vec2<f32>, radius: f32) -> f32 {
    let extent = size * 0.5f;
    let r = clamp(radius, 0.0f, min(extent.x, extent.y));
    let q = abs(p - extent) - (extent - vec2<f32>(r));
    return length(max(q, vec2<f32>(0.0f))) + min(max(q.x, q.y), 0.0f) - r;
}

";

pub(super) fn module(g: &mut Gen<'_>) {
    // Derivatives and samples may sit under per-fragment branches the
    // program wrote; the uniformity analysis would reject them.
    g.out
        .push_str("diagnostic(off, derivative_uniformity);\n\n");
    g.out.push_str(HELPERS);
    for (index, record) in g.p.records.iter().enumerate() {
        put(&mut g.out, format_args!("struct R{index} {{\n"));
        for (f, field) in record.fields.iter().enumerate() {
            put(&mut g.out, format_args!("    f{f}: {},\n", ty(field.ty)));
        }
        g.out.push_str("}\n\n");
    }
    g.out
        .push_str("struct VertexOutput {\n    clip_position: vec4<f32>,\n}\n\n");

    let uniforms = &g.i.uniforms;
    if !uniforms.fields.is_empty() {
        g.out.push_str("struct Uniforms {\n");
        for (k, f) in uniforms.fields.iter().enumerate() {
            let next = uniforms
                .fields
                .get(k + 1)
                .map_or(uniforms.size, |n| n.offset);
            let span = next - f.offset;
            let size = if span == f.size {
                String::new()
            } else {
                format!("@size({span}) ")
            };
            put(&mut g.out, format_args!("    {size}u{k}: {},\n", ty(f.ty)));
        }
        g.out
            .push_str("}\n\n@group(0) @binding(0) var<uniform> u: Uniforms;\n\n");
    }
    let instance = &g.i.instance;
    if !instance.fields.is_empty() {
        g.out.push_str("struct Instance {\n");
        for (k, f) in instance.fields.iter().enumerate() {
            for j in 0..leaf_lanes(f.ty) {
                put(
                    &mut g.out,
                    format_args!("    i{k}_{j}: {},\n", lane_type(f.ty)),
                );
            }
        }
        g.out.push_str(
            "}\n\n@group(0) @binding(1) var<storage, read> instances: array<Instance>;\n\n",
        );
    }
    let textures = g.i.textures.len();
    for t in &g.i.textures {
        put(
            &mut g.out,
            format_args!(
                "@group(1) @binding({0}) var t{0}: texture_2d<f32>;\n",
                t.index
            ),
        );
    }
    for s in &g.i.samplers {
        put(
            &mut g.out,
            format_args!(
                "@group(1) @binding({}) var s{}: sampler;\n",
                textures as u32 + s.index,
                s.index
            ),
        );
    }
    if textures + g.i.samplers.len() > 0 {
        g.out.push('\n');
    }

    g.out
        .push_str("struct VOut {\n    @builtin(position) clip_position: vec4<f32>,\n");
    let mut location = 0;
    for v in &g.i.varyings {
        let flat = if v.interpolation == Interpolation::Flat {
            " @interpolate(flat)"
        } else {
            ""
        };
        put(
            &mut g.out,
            format_args!(
                "    @location({location}){flat} v{}: {},\n",
                v.offset,
                ty(v.ty)
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
                    "    @location({location}) @interpolate(flat) {name}: {},\n",
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
    g.out.push_str("}\n\n");

    g.functions();
    let p = g.p;
    if let Some(f) = &p.vertex {
        put(
            &mut g.out,
            format_args!(
                "@vertex\nfn {}(@builtin(vertex_index) vid: u32, @builtin(instance_index) iid: u32) -> VOut {{\n",
                entry_name(Stage::Vertex)
            ),
        );
        g.entry_body(f);
    }
    if let Some(f) = &p.fragment {
        put(
            &mut g.out,
            format_args!(
                "@fragment\nfn {}(vin: VOut) -> @location(0) vec4<f32> {{\n",
                entry_name(Stage::Fragment)
            ),
        );
        g.entry_body(f);
    }
}
