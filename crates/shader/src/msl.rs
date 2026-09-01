//! Hand-written MSL, keyed per primitive (plan step 9, layer D).
//!
//! The full shader DSL → multi-backend compiler (§36) is out of scope for
//! Phase 2. Until it lands, each built-in primitive carries a hand-written
//! Metal source string plus the [`InstanceSchema`] it expects. The two are the
//! shader half of the three-way instance contract (the other two halves are the
//! `#[derive(GpuInstance)]` layout and the headless rasterizer's field reader);
//! `create_pipeline` validates the derived layout against the schema at
//! registration.
//!
//! ## Metal instance-struct packing (ported from makepad, native rewrite)
//!
//! makepad's `metal_create_instance_struct` (`platform/script/src/shader_metal.rs`)
//! emits the raw per-instance buffer struct with **packed** vector types
//! (`packed_float2/3/4`) so it matches the CPU-side `#[repr(C)]` layout with no
//! inter-field padding; scalars (`float`/`uint`) stay unpacked (they are already
//! 4-byte-aligned). A `mat4x4` is split into four `packed_float4` columns. We
//! reproduce that rule by hand here: every `[f32; N>=2]` field is `packed_floatN`
//! and every scalar is a bare `float`, in `#[repr(C)]` field order.
//!
//! Reserved-word caveat: `half` is an MSL type (16-bit float) and cannot be used
//! as an identifier — see the `viso-msl-reserved-half` note.

use viso_gpu::{AttrFormat, InstanceSchema, SchemaAttr};

/// The built-in primitive shaders (§15.3). One entry per [`Primitive`] kind in
/// `viso-render`; each maps to a hand-written MSL program and an instance
/// schema.
///
/// Quad, Image, GlyphRun, Path, and Mesh are implemented in this Phase 2 slice;
/// [`PrimitiveKind::Layer`] returns `None` from
/// [`shader_source`]/[`instance_schema`] (it is a clip container, not a shaded
/// primitive — see the renderer's scissor handling).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PrimitiveKind {
    /// A rounded/bordered rectangle.
    Quad,
    /// A run of shaped glyphs.
    GlyphRun,
    /// A textured image.
    Image,
    /// A filled/stroked vector path.
    Path,
    /// A colored triangle mesh.
    Mesh,
    /// An offscreen-composited layer.
    Layer,
}

/// The hand-written Metal source for `kind`, or `None` if that primitive has no
/// shader yet.
pub fn shader_source(kind: PrimitiveKind) -> Option<&'static str> {
    match kind {
        PrimitiveKind::Quad => Some(QUAD_MSL),
        PrimitiveKind::Image => Some(IMAGE_MSL),
        PrimitiveKind::GlyphRun => Some(GLYPHRUN_MSL),
        // Path and Mesh share the general per-vertex mesh pipeline.
        PrimitiveKind::Path | PrimitiveKind::Mesh => Some(MESH_MSL),
        _ => None,
    }
}

/// The instance schema `kind`'s shader declares, or `None` if that primitive is
/// not implemented yet. This is what the pipeline validates the derived
/// `GpuInstance` layout against at registration.
pub fn instance_schema(kind: PrimitiveKind) -> Option<InstanceSchema> {
    match kind {
        PrimitiveKind::Quad => Some(quad_schema()),
        PrimitiveKind::Image => Some(image_schema()),
        PrimitiveKind::GlyphRun => Some(glyphrun_schema()),
        // Path and Mesh validate their per-vertex layout against `mesh_schema`.
        PrimitiveKind::Path | PrimitiveKind::Mesh => Some(mesh_schema()),
        _ => None,
    }
}

/// The instance schema the Quad shader declares. Single source of truth for the
/// Quad field contract; `viso-render`'s `QuadInstance` derive is validated
/// against it, and the fields map 1:1 onto [`QUAD_MSL`]'s `InstanceIn`.
pub fn quad_schema() -> InstanceSchema {
    InstanceSchema {
        attributes: &[
            SchemaAttr {
                name: "rect_pos",
                format: AttrFormat::Float2,
            },
            SchemaAttr {
                name: "rect_size",
                format: AttrFormat::Float2,
            },
            SchemaAttr {
                name: "color",
                format: AttrFormat::Float4,
            },
            SchemaAttr {
                name: "radius",
                format: AttrFormat::Float1,
            },
            SchemaAttr {
                name: "border_width",
                format: AttrFormat::Float1,
            },
            SchemaAttr {
                name: "border_color",
                format: AttrFormat::Float4,
            },
        ],
    }
}

/// Inline MSL for the Quad built-in (Metal backend).
///
/// The headless raster backend ignores this and dispatches on
/// `BuiltinShader::Quad`; only the real Metal backend compiles it, so MSL
/// syntax/reserved-word errors surface only when a device pipeline is created
/// (run `viso-example-hello-world`). See `viso-msl-reserved-half`.
///
/// Contract (must stay in lockstep with `QuadInstance` / [`quad_schema`] and the
/// headless `fill_quad`):
/// - Per-instance data is a raw buffer at index 1 (`InstanceIn`, field order and
///   `packed_float*` types matching the `#[repr(C)]` struct exactly).
/// - The viewport size `[width, height]` is an inline uniform at index 0.
/// - Six `vertex_id`s form two triangles covering the rect (plus 1px AA pad).
/// - Colors are **straight** in the instance; the fragment shader premultiplies
///   its output (blend is `src One`, `dst OneMinusSourceAlpha`).
/// - Rounded-rect SDF, linear-coverage AA, and border-over-fill reproduce the
///   headless math so Metal and headless agree within tolerance.
pub const QUAD_MSL: &str = r#"
#include <metal_stdlib>
using namespace metal;

struct InstanceIn {
    packed_float2 rect_pos;
    packed_float2 rect_size;
    packed_float4 color;
    float radius;
    float border_width;
    packed_float4 border_color;
};

struct Uniforms {
    packed_float2 viewport;
};

struct VOut {
    float4 position [[position]];
    float2 local;        // pixel-space position relative to the padded rect
    float2 half_size;    // half extents of the rect (pixels)
    float2 center;       // rect center (pixels)
    float radius;
    float border_width;
    float4 color;
    float4 border_color;
};

vertex VOut vertex_main(uint vid [[vertex_id]],
                        uint iid [[instance_id]],
                        const device InstanceIn* instances [[buffer(1)]],
                        constant Uniforms& u [[buffer(0)]]) {
    InstanceIn inst = instances[iid];

    // Two triangles: (0,0)(1,0)(0,1) and (1,0)(1,1)(0,1). Pad by 1px each side
    // so the AA ramp at the rect edge is covered.
    float2 corner;
    switch (vid) {
        case 0: corner = float2(0.0, 0.0); break;
        case 1: corner = float2(1.0, 0.0); break;
        case 2: corner = float2(0.0, 1.0); break;
        case 3: corner = float2(1.0, 0.0); break;
        case 4: corner = float2(1.0, 1.0); break;
        default: corner = float2(0.0, 1.0); break;
    }

    float2 pos = float2(inst.rect_pos);
    float2 size = float2(inst.rect_size);
    float2 pad = float2(1.0, 1.0);
    float2 pixel = pos - pad + corner * (size + 2.0 * pad);

    // Pixel-space (top-left origin) → NDC. Y is flipped for Metal.
    float2 vp = float2(u.viewport);
    float2 ndc = float2(pixel.x / vp.x * 2.0 - 1.0,
                        1.0 - pixel.y / vp.y * 2.0);

    VOut out;
    out.position = float4(ndc, 0.0, 1.0);
    out.local = pixel;
    out.half_size = size * 0.5;
    out.center = pos + size * 0.5;
    out.radius = inst.radius;
    out.border_width = inst.border_width;
    out.color = float4(inst.color);
    out.border_color = float4(inst.border_color);
    return out;
}

// Signed distance to a rounded box (IQ), negative inside. `k` is the doubled,
// clamped corner radius.
// `half_ext` is the box's half-extents. (Do not name it `half` — that is a
// reserved MSL type name, the 16-bit float.)
static inline float box_sdf(float2 p, float2 center, float2 half_ext, float k) {
    float2 q = abs(p - center) - (half_ext - k);
    float2 mx = max(q, float2(0.0));
    return length(mx) + min(max(q.x, q.y), 0.0) - k;
}

fragment float4 fragment_main(VOut in [[stage_in]]) {
    float k = min(2.0 * in.radius, min(in.half_size.x, in.half_size.y));
    float d = box_sdf(in.local, in.center, in.half_size, k);

    // aa ~= 1 at 1:1 scale: linear coverage over ~1px.
    float fill_cov = clamp(-d, 0.0, 1.0);

    // Fill, premultiplied.
    float fa = in.color.a * fill_cov;
    float4 src = float4(in.color.rgb * fa, fa);

    // Border over fill (both premultiplied source-over).
    if (in.border_width > 0.0) {
        float bcov = clamp(-(abs(d) - in.border_width * 0.5), 0.0, 1.0);
        if (bcov > 0.0) {
            float ba = in.border_color.a * bcov;
            float4 bsrc = float4(in.border_color.rgb * ba, ba);
            src = bsrc + src * (1.0 - ba);
        }
    }
    return src;
}
"#;

/// The instance schema the Image shader declares. Single source of truth for the
/// Image field contract; `viso-render`'s `ImageInstance` derive is validated
/// against it, and the fields map 1:1 onto [`IMAGE_MSL`]'s `InstanceIn`.
///
/// Unlike makepad's `DrawImage` (which derives the UV from the unit-quad corner
/// and has no atlas sub-rect), Viso carries an explicit per-instance UV sub-rect
/// (`uv_pos`/`uv_size`) so the same path serves atlas/glyph sub-regions later.
pub fn image_schema() -> InstanceSchema {
    InstanceSchema {
        attributes: &[
            SchemaAttr {
                name: "rect_pos",
                format: AttrFormat::Float2,
            },
            SchemaAttr {
                name: "rect_size",
                format: AttrFormat::Float2,
            },
            SchemaAttr {
                name: "uv_pos",
                format: AttrFormat::Float2,
            },
            SchemaAttr {
                name: "uv_size",
                format: AttrFormat::Float2,
            },
            SchemaAttr {
                name: "color",
                format: AttrFormat::Float4,
            },
        ],
    }
}

/// Inline MSL for the Image built-in (Metal backend).
///
/// Like [`QUAD_MSL`], the headless backend ignores this and dispatches on
/// `BuiltinShader::Image`; only the real Metal backend compiles it, so MSL
/// errors surface only when a device pipeline is created (run
/// `viso-example-hello-world`). See `viso-msl-reserved-half`.
///
/// Contract (must stay in lockstep with `ImageInstance` / [`image_schema`] and
/// the headless `fill_image`):
/// - Per-instance data is a raw buffer at index 1 (`InstanceIn`, `packed_float*`
///   types matching the `#[repr(C)]` struct exactly).
/// - The viewport size `[width, height]` is an inline uniform at index 0.
/// - The bound texture is at `[[texture(0)]]`, its sampler at `[[sampler(0)]]`.
/// - Six `vertex_id`s form two triangles covering the rect (no AA pad — the
///   image samples exactly its rect); the UV interpolates across the sub-rect
///   `uv_pos + corner * uv_size`.
/// - `color` is a **straight** tint (a = opacity); the sampled texel is assumed
///   premultiplied (Viso textures store premultiplied linear), so the fragment
///   multiplies the premultiplied texel by the straight tint's premultiplied
///   form and outputs premultiplied (blend is `src One`, `dst
///   OneMinusSourceAlpha`).
pub const IMAGE_MSL: &str = r#"
#include <metal_stdlib>
using namespace metal;

struct InstanceIn {
    packed_float2 rect_pos;
    packed_float2 rect_size;
    packed_float2 uv_pos;
    packed_float2 uv_size;
    packed_float4 color;
};

struct Uniforms {
    packed_float2 viewport;
};

struct VOut {
    float4 position [[position]];
    float2 uv;
    float4 tint;
};

vertex VOut vertex_main(uint vid [[vertex_id]],
                        uint iid [[instance_id]],
                        const device InstanceIn* instances [[buffer(1)]],
                        constant Uniforms& u [[buffer(0)]]) {
    InstanceIn inst = instances[iid];

    float2 corner;
    switch (vid) {
        case 0: corner = float2(0.0, 0.0); break;
        case 1: corner = float2(1.0, 0.0); break;
        case 2: corner = float2(0.0, 1.0); break;
        case 3: corner = float2(1.0, 0.0); break;
        case 4: corner = float2(1.0, 1.0); break;
        default: corner = float2(0.0, 1.0); break;
    }

    float2 pos = float2(inst.rect_pos);
    float2 size = float2(inst.rect_size);
    float2 pixel = pos + corner * size;

    float2 vp = float2(u.viewport);
    float2 ndc = float2(pixel.x / vp.x * 2.0 - 1.0,
                        1.0 - pixel.y / vp.y * 2.0);

    VOut out;
    out.position = float4(ndc, 0.0, 1.0);
    out.uv = float2(inst.uv_pos) + corner * float2(inst.uv_size);
    out.tint = float4(inst.color);
    return out;
}

fragment float4 fragment_main(VOut in [[stage_in]],
                              texture2d<float> tex [[texture(0)]],
                              sampler samp [[sampler(0)]]) {
    // Texel is premultiplied linear (Viso texture convention). Scale it by the
    // straight tint's premultiplied form: rgb by (tint.rgb * tint.a), a by
    // tint.a — keeping the result premultiplied.
    float4 texel = tex.sample(samp, in.uv);
    float4 t = float4(in.tint.rgb * in.tint.a, in.tint.a);
    return texel * t;
}
"#;

/// The instance schema the GlyphRun shader declares. Single source of truth for
/// the GlyphRun field contract; `viso-render`'s `GlyphInstance` derive is
/// validated against it, and the fields map 1:1 onto [`GLYPHRUN_MSL`]'s
/// `InstanceIn`.
///
/// Structurally this is the Image schema plus a per-instance `px_range`: a glyph
/// is a textured rect sampling the single-channel R8 SDF atlas, and `px_range`
/// tells the fragment shader how many stored-units span one screen pixel so it
/// can turn the sampled signed distance back into antialiased coverage.
pub fn glyphrun_schema() -> InstanceSchema {
    InstanceSchema {
        attributes: &[
            SchemaAttr {
                name: "rect_pos",
                format: AttrFormat::Float2,
            },
            SchemaAttr {
                name: "rect_size",
                format: AttrFormat::Float2,
            },
            SchemaAttr {
                name: "uv_pos",
                format: AttrFormat::Float2,
            },
            SchemaAttr {
                name: "uv_size",
                format: AttrFormat::Float2,
            },
            SchemaAttr {
                name: "color",
                format: AttrFormat::Float4,
            },
            SchemaAttr {
                name: "px_range",
                format: AttrFormat::Float1,
            },
        ],
    }
}

/// Inline MSL for the GlyphRun built-in (Metal backend).
///
/// Like [`QUAD_MSL`]/[`IMAGE_MSL`], the headless backend ignores this and
/// dispatches on `BuiltinShader::GlyphRun`; only the real Metal backend compiles
/// it, so MSL errors surface only when a device pipeline is created (run
/// `viso-example-hello-world`). See `viso-msl-reserved-half`.
///
/// Contract (must stay in lockstep with `GlyphInstance` / [`glyphrun_schema`] and
/// the headless `fill_glyph`):
/// - Per-instance data is a raw buffer at index 1 (`InstanceIn`, `packed_float*`
///   types matching the `#[repr(C)]` struct exactly).
/// - The viewport size `[width, height]` is an inline uniform at index 0.
/// - The bound atlas is a single-channel R8 texture at `[[texture(0)]]`, its
///   linear-clamp sampler at `[[sampler(0)]]` (SDF sampling needs bilinear).
/// - Six `vertex_id`s form two triangles covering the rect; the UV interpolates
///   across the sub-rect `uv_pos + corner * uv_size`.
/// - The atlas stores an ESDT signed distance: `stored = 1 - (d/radius + cutoff)`
///   with the glyph edge (`d = 0`) at `stored = 0.75` (`1 - cutoff`, cutoff fixed
///   at 0.25 in `viso-text`'s rasterizer). Coverage is recovered as
///   `clamp((sd - 0.75) * px_range + 0.5, 0, 1)`. This must match the headless
///   `fill_glyph` decode exactly.
/// - `color` is **straight** RGBA; the fragment premultiplies by coverage and
///   outputs premultiplied (blend is `src One`, `dst OneMinusSourceAlpha`).
pub const GLYPHRUN_MSL: &str = r#"
#include <metal_stdlib>
using namespace metal;

struct InstanceIn {
    packed_float2 rect_pos;
    packed_float2 rect_size;
    packed_float2 uv_pos;
    packed_float2 uv_size;
    packed_float4 color;
    float px_range;
};

struct Uniforms {
    packed_float2 viewport;
};

struct VOut {
    float4 position [[position]];
    float2 uv;
    float4 color;
    float px_range;
};

vertex VOut vertex_main(uint vid [[vertex_id]],
                        uint iid [[instance_id]],
                        const device InstanceIn* instances [[buffer(1)]],
                        constant Uniforms& u [[buffer(0)]]) {
    InstanceIn inst = instances[iid];

    float2 corner;
    switch (vid) {
        case 0: corner = float2(0.0, 0.0); break;
        case 1: corner = float2(1.0, 0.0); break;
        case 2: corner = float2(0.0, 1.0); break;
        case 3: corner = float2(1.0, 0.0); break;
        case 4: corner = float2(1.0, 1.0); break;
        default: corner = float2(0.0, 1.0); break;
    }

    float2 pos = float2(inst.rect_pos);
    float2 size = float2(inst.rect_size);
    float2 pixel = pos + corner * size;

    float2 vp = float2(u.viewport);
    float2 ndc = float2(pixel.x / vp.x * 2.0 - 1.0,
                        1.0 - pixel.y / vp.y * 2.0);

    VOut out;
    out.position = float4(ndc, 0.0, 1.0);
    out.uv = float2(inst.uv_pos) + corner * float2(inst.uv_size);
    out.color = float4(inst.color);
    out.px_range = inst.px_range;
    return out;
}

fragment float4 fragment_main(VOut in [[stage_in]],
                              texture2d<float> tex [[texture(0)]],
                              sampler samp [[sampler(0)]]) {
    // Decode the ESDT single-channel SDF back to coverage. The glyph edge sits
    // at stored 0.75 (= 1 - cutoff, cutoff fixed at 0.25 in the rasterizer);
    // `px_range` stored-units span one screen pixel of the coverage ramp.
    float sd = tex.sample(samp, in.uv).r;
    float cov = clamp((sd - 0.75) * in.px_range + 0.5, 0.0, 1.0);
    float a = in.color.a * cov;
    return float4(in.color.rgb * a, a);
}
"#;

/// The per-vertex schema the general mesh shader declares. Single source of
/// truth for the mesh vertex contract, shared by Path and Mesh; `viso-render`'s
/// `MeshVertex` derive is validated against it, and the fields map 1:1 onto
/// [`MESH_MSL`]'s `VertexIn`.
///
/// Unlike the Quad/Image schemas — which describe *per-instance* data — this
/// describes *per-vertex* data: the mesh pipeline draws a real indexed
/// triangle list from a vertex buffer, not a `vertex_id`-generated quad. The
/// same 4-byte-aligned attribute descriptors serve both (a vertex layout is
/// structurally identical to an instance layout).
pub fn mesh_schema() -> InstanceSchema {
    InstanceSchema {
        attributes: &[
            SchemaAttr {
                name: "pos",
                format: AttrFormat::Float2,
            },
            SchemaAttr {
                name: "color",
                format: AttrFormat::Float4,
            },
            SchemaAttr {
                name: "edge",
                format: AttrFormat::Float1,
            },
        ],
    }
}

/// Inline MSL for the general mesh built-in (Path/Mesh), Metal backend.
///
/// Unlike [`QUAD_MSL`]/[`IMAGE_MSL`], this reads a **real per-vertex buffer**
/// at index 0 indexed by `vertex_id` (the CPU tessellator emits absolute
/// indices into that buffer via `drawIndexedPrimitives`), rather than
/// synthesizing a unit quad. There is no per-instance buffer.
///
/// Like the other built-ins, only the real Metal backend compiles this, so MSL
/// errors surface only when a device pipeline is created (run
/// `viso-example-hello-world`). See `viso-msl-reserved-half`.
///
/// Contract (must stay in lockstep with `MeshVertex` / [`mesh_schema`] and the
/// headless `fill_mesh`):
/// - Per-vertex data is a raw buffer at **vertex buffer index 0** (`VertexIn`,
///   field order and `packed_float*` types matching the `#[repr(C)]` struct).
/// - The viewport size `[width, height]` is an inline uniform at **buffer index
///   1** (the vertex buffer already occupies index 0, so mesh uniforms move to 1
///   — unlike the quad/image built-ins where uniforms are at index 0 and the
///   instance buffer at index 1).
/// - `pos` is in physical pixels (top-left origin), mapped to NDC with a Y flip.
/// - `color` is **straight** linear RGBA; `edge` is a `[0, 1]` coverage weight
///   (1 in the interior, ramping to 0 at antialiased fringe vertices). The
///   fragment multiplies alpha by the interpolated `edge` and premultiplies.
pub const MESH_MSL: &str = r#"
#include <metal_stdlib>
using namespace metal;

struct VertexIn {
    packed_float2 pos;
    packed_float4 color;
    float edge;
};

struct Uniforms {
    packed_float2 viewport;
};

struct VOut {
    float4 position [[position]];
    float4 color;
    float edge;
};

vertex VOut vertex_main(uint vid [[vertex_id]],
                        const device VertexIn* verts [[buffer(0)]],
                        constant Uniforms& u [[buffer(1)]]) {
    VertexIn v = verts[vid];

    float2 pixel = float2(v.pos);
    float2 vp = float2(u.viewport);
    float2 ndc = float2(pixel.x / vp.x * 2.0 - 1.0,
                        1.0 - pixel.y / vp.y * 2.0);

    VOut out;
    out.position = float4(ndc, 0.0, 1.0);
    out.color = float4(v.color);
    out.edge = v.edge;
    return out;
}

fragment float4 fragment_main(VOut in [[stage_in]]) {
    float cov = clamp(in.edge, 0.0, 1.0);
    float a = in.color.a * cov;
    return float4(in.color.rgb * a, a);
}
"#;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quad_has_source_and_schema() {
        assert!(shader_source(PrimitiveKind::Quad).is_some());
        assert!(instance_schema(PrimitiveKind::Quad).is_some());
        // Schema field order matches the MSL `InstanceIn` field order.
        let schema = quad_schema();
        let names: Vec<_> = schema.attributes.iter().map(|a| a.name).collect();
        assert_eq!(
            names,
            [
                "rect_pos",
                "rect_size",
                "color",
                "radius",
                "border_width",
                "border_color"
            ]
        );
    }

    #[test]
    fn image_has_source_and_schema() {
        assert!(shader_source(PrimitiveKind::Image).is_some());
        assert!(instance_schema(PrimitiveKind::Image).is_some());
        // Schema field order matches the MSL `InstanceIn` field order.
        let names: Vec<_> = image_schema().attributes.iter().map(|a| a.name).collect();
        assert_eq!(
            names,
            ["rect_pos", "rect_size", "uv_pos", "uv_size", "color"]
        );
    }

    #[test]
    fn mesh_has_source_and_schema() {
        // Path and Mesh share the general per-vertex mesh pipeline.
        for kind in [PrimitiveKind::Path, PrimitiveKind::Mesh] {
            assert!(shader_source(kind).is_some());
            assert!(instance_schema(kind).is_some());
        }
        // Schema field order matches the MSL `VertexIn` field order.
        let names: Vec<_> = mesh_schema().attributes.iter().map(|a| a.name).collect();
        assert_eq!(names, ["pos", "color", "edge"]);
    }

    #[test]
    fn glyphrun_has_source_and_schema() {
        assert!(shader_source(PrimitiveKind::GlyphRun).is_some());
        assert!(instance_schema(PrimitiveKind::GlyphRun).is_some());
        // Schema field order matches the MSL `InstanceIn` field order.
        let names: Vec<_> = glyphrun_schema()
            .attributes
            .iter()
            .map(|a| a.name)
            .collect();
        assert_eq!(
            names,
            [
                "rect_pos",
                "rect_size",
                "uv_pos",
                "uv_size",
                "color",
                "px_range"
            ]
        );
    }

    #[test]
    fn layer_has_no_shader() {
        // Layer is a rectangular scissor clip, not a drawn primitive, so it has
        // neither an MSL source nor an instance schema.
        assert!(shader_source(PrimitiveKind::Layer).is_none());
        assert!(instance_schema(PrimitiveKind::Layer).is_none());
    }
}
