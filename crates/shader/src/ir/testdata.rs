//! Frozen copies of the original hand-written built-in MSL, kept only for tests.
//!
//! Before Slice Q these four strings were the shipping source of truth in
//! `msl.rs`; now the MSL is derived from the typed [`ir`](crate::ir). These frozen
//! literals let the codegen tests assert the derived MSL is *byte-for-byte* the
//! historical text — an independent oracle, not a tautology against the live
//! `msl.rs` accessors (which now project from the same IR the codegen does).
//!
//! If a body fragment is intentionally changed, update the matching literal here
//! and verify on a real Metal device (the headless backend does not compile MSL;
//! see `viso-msl-reserved-half`).

/// The original hand-written QUAD MSL, frozen as a codegen oracle.
pub const QUAD_MSL_ORIGINAL: &str = r##"
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

// Device-pixel coverage factor: how many SDF units span one screen pixel at the
// current sampling position, inverted. Coverage ramps over ~1 device pixel
// regardless of scale, so the AA width tracks the physical grid.
static inline float aa_factor(float2 p) {
    return 1.0 / length(float2(length(dfdx(p)), length(dfdy(p))));
}

fragment float4 fragment_main(VOut in [[stage_in]]) {
    float k = min(2.0 * in.radius, min(in.half_size.x, in.half_size.y));
    float d = box_sdf(in.local, in.center, in.half_size, k);

    // Device-pixel-aware coverage: linear ramp over ~1 physical pixel.
    float aa = aa_factor(in.local);
    float fill_cov = clamp(-d * aa, 0.0, 1.0);

    // Fill, premultiplied.
    float fa = in.color.a * fill_cov;
    float4 src = float4(in.color.rgb * fa, fa);

    // Border over fill (both premultiplied source-over).
    if (in.border_width > 0.0) {
        float bcov = clamp(-(abs(d) - in.border_width * 0.5) * aa, 0.0, 1.0);
        if (bcov > 0.0) {
            float ba = in.border_color.a * bcov;
            float4 bsrc = float4(in.border_color.rgb * ba, ba);
            src = bsrc + src * (1.0 - ba);
        }
    }
    return src;
}
"##;

/// The original hand-written IMAGE MSL, frozen as a codegen oracle.
pub const IMAGE_MSL_ORIGINAL: &str = r##"
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
"##;

/// The intended GLYPHRUN MSL, frozen as a codegen oracle.
///
/// The glyph path samples a single-channel A8 coverage atlas directly (`cov =
/// texel.r`); there is no signed-distance decode and no per-instance `px_range`.
/// This text is the on-device-verified target, not a pre-migration baseline.
pub const GLYPHRUN_MSL_ORIGINAL: &str = r##"
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
    float4 color;
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
    return out;
}

fragment float4 fragment_main(VOut in [[stage_in]],
                              texture2d<float> tex [[texture(0)]],
                              sampler samp [[sampler(0)]]) {
    // Single-channel A8 coverage sampled directly: the atlas texel's red channel
    // is exact per-pixel coverage. Modulate the run color by it, premultiplied.
    float cov = tex.sample(samp, in.uv).r;
    float a = in.color.a * cov;
    return float4(in.color.rgb * a, a);
}
"##;

/// The original hand-written MESH MSL, frozen as a codegen oracle.
pub const MESH_MSL_ORIGINAL: &str = r##"
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
"##;

/// The AnalyticRRect MSL, frozen as a codegen oracle.
///
/// Unlike the four pre-existing built-ins, this family has no pre-migration
/// hand-written source: it was introduced directly on the typed IR, so its oracle
/// is the codegen output itself, locked by the byte-equivalence self-consistency
/// test. It still needs a one-time real-Metal compile to trust (the headless
/// backend does not compile MSL; see `viso-msl-reserved-half`).
pub const ANALYTIC_RRECT_MSL_ORIGINAL: &str = r##"
#include <metal_stdlib>
using namespace metal;

struct InstanceIn {
    packed_float2 rect_pos;
    packed_float2 rect_size;
    packed_float4 color;
    packed_float4 radius;
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
    float4 radius;       // per-corner radius: lt, rt, rb, lb
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
    out.radius = float4(inst.radius);
    out.border_width = inst.border_width;
    out.color = float4(inst.color);
    out.border_color = float4(inst.border_color);
    return out;
}

// Signed distance to a rounded box with an independent radius per corner
// (`radii` ordered left-top, right-top, right-bottom, left-bottom), negative
// inside. The active corner's radius is selected by the sample's quadrant, then
// clamped to the box (a radius cannot exceed the smaller full extent).
// `half_ext` is the box's half-extents. (Do not name it `half` — that is a
// reserved MSL type name, the 16-bit float.)
static inline float rrect_sdf(float2 p, float2 center, float2 half_ext, float4 radii) {
    float2 d = p - center;
    // Quadrant select: x<0 picks a left corner, y<0 picks a top corner.
    float r = d.x < 0.0 ? (d.y < 0.0 ? radii.x : radii.w)
                        : (d.y < 0.0 ? radii.y : radii.z);
    float k = min(2.0 * r, min(half_ext.x, half_ext.y));
    float2 q = abs(d) - (half_ext - k);
    float2 mx = max(q, float2(0.0));
    return length(mx) + min(max(q.x, q.y), 0.0) - k;
}

// Device-pixel coverage factor: how many SDF units span one screen pixel at the
// current sampling position, inverted. Coverage ramps over ~1 device pixel
// regardless of scale, so the AA width tracks the physical grid.
static inline float aa_factor(float2 p) {
    return 1.0 / length(float2(length(dfdx(p)), length(dfdy(p))));
}

fragment float4 fragment_main(VOut in [[stage_in]]) {
    float d = rrect_sdf(in.local, in.center, in.half_size, in.radius);

    // Device-pixel-aware coverage: linear ramp over ~1 physical pixel.
    float aa = aa_factor(in.local);
    float fill_cov = clamp(-d * aa, 0.0, 1.0);

    // Fill, premultiplied.
    float fa = in.color.a * fill_cov;
    float4 src = float4(in.color.rgb * fa, fa);

    // Border over fill (both premultiplied source-over).
    if (in.border_width > 0.0) {
        float bcov = clamp(-(abs(d) - in.border_width * 0.5) * aa, 0.0, 1.0);
        if (bcov > 0.0) {
            float ba = in.border_color.a * bcov;
            float4 bsrc = float4(in.border_color.rgb * ba, ba);
            src = bsrc + src * (1.0 - ba);
        }
    }
    return src;
}
"##;

/// The AnalyticEllipse MSL, frozen as a codegen oracle. Same provenance as
/// [`ANALYTIC_RRECT_MSL_ORIGINAL`].
pub const ANALYTIC_ELLIPSE_MSL_ORIGINAL: &str = r##"
#include <metal_stdlib>
using namespace metal;

struct InstanceIn {
    packed_float2 rect_pos;
    packed_float2 rect_size;
    packed_float4 color;
    float border_width;
    packed_float4 border_color;
};

struct Uniforms {
    packed_float2 viewport;
};

struct VOut {
    float4 position [[position]];
    float2 local;        // pixel-space position relative to the padded rect
    float2 half_size;    // ellipse radii = half extents (pixels)
    float2 center;       // ellipse center (pixels)
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
    // so the AA ramp at the ellipse edge is covered.
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
    out.border_width = inst.border_width;
    out.color = float4(inst.color);
    out.border_color = float4(inst.border_color);
    return out;
}

// Signed distance to an axis-aligned ellipse, negative inside. The point is
// normalized by the per-axis radii and offset by the unit circle, then scaled
// back by the smaller radius to approximate pixel-space distance for the AA
// ramp. `radii` are the ellipse's half-extents.
static inline float ellipse_sdf(float2 p, float2 center, float2 radii) {
    float2 n = (p - center) / radii;
    return (length(n) - 1.0) * min(radii.x, radii.y);
}

// Device-pixel coverage factor: how many SDF units span one screen pixel at the
// current sampling position, inverted. Coverage ramps over ~1 device pixel
// regardless of scale, so the AA width tracks the physical grid.
static inline float aa_factor(float2 p) {
    return 1.0 / length(float2(length(dfdx(p)), length(dfdy(p))));
}

fragment float4 fragment_main(VOut in [[stage_in]]) {
    float d = ellipse_sdf(in.local, in.center, in.half_size);

    // Device-pixel-aware coverage: linear ramp over ~1 physical pixel.
    float aa = aa_factor(in.local);
    float fill_cov = clamp(-d * aa, 0.0, 1.0);

    // Fill, premultiplied.
    float fa = in.color.a * fill_cov;
    float4 src = float4(in.color.rgb * fa, fa);

    // Border over fill (both premultiplied source-over).
    if (in.border_width > 0.0) {
        float bcov = clamp(-(abs(d) - in.border_width * 0.5) * aa, 0.0, 1.0);
        if (bcov > 0.0) {
            float ba = in.border_color.a * bcov;
            float4 bsrc = float4(in.border_color.rgb * ba, ba);
            src = bsrc + src * (1.0 - ba);
        }
    }
    return src;
}
"##;

/// The AnalyticCapsule MSL, frozen as a codegen oracle. Same provenance as
/// [`ANALYTIC_RRECT_MSL_ORIGINAL`]: a new family with no historical hand-
/// written Metal, so the oracle is the codegen output itself, locked by the
/// byte-equivalence self-check in `codegen_msl`.
pub const ANALYTIC_CAPSULE_MSL_ORIGINAL: &str = r##"
#include <metal_stdlib>
using namespace metal;

struct InstanceIn {
    packed_float2 rect_pos;
    packed_float2 rect_size;
    packed_float4 color;
    float border_width;
    packed_float4 border_color;
};

struct Uniforms {
    packed_float2 viewport;
};

struct VOut {
    float4 position [[position]];
    float2 local;        // pixel-space position relative to the padded rect
    float2 half_size;    // box half extents (pixels); capsule radius = min of the two
    float2 center;       // capsule center (pixels)
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
    // so the AA ramp at the capsule edge is covered.
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
    out.border_width = inst.border_width;
    out.color = float4(inst.color);
    out.border_color = float4(inst.border_color);
    return out;
}

// Signed distance to a capsule (stadium), negative inside. The corner radius is
// the smaller half-extent, so the short axis rounds into a semicircle and the
// long axis stays straight — a pill. This is the rounded-box SDF with the radius
// pinned to `min(half_ext.x, half_ext.y)`. `half_ext` is the box's half-extents.
// (Do not name it `half` — that is a reserved MSL type name, the 16-bit float.)
static inline float capsule_sdf(float2 p, float2 center, float2 half_ext) {
    float k = min(half_ext.x, half_ext.y);
    float2 q = abs(p - center) - (half_ext - k);
    float2 mx = max(q, float2(0.0));
    return length(mx) + min(max(q.x, q.y), 0.0) - k;
}

// Device-pixel coverage factor: how many SDF units span one screen pixel at the
// current sampling position, inverted. Coverage ramps over ~1 device pixel
// regardless of scale, so the AA width tracks the physical grid.
static inline float aa_factor(float2 p) {
    return 1.0 / length(float2(length(dfdx(p)), length(dfdy(p))));
}

fragment float4 fragment_main(VOut in [[stage_in]]) {
    float d = capsule_sdf(in.local, in.center, in.half_size);

    // Device-pixel-aware coverage: linear ramp over ~1 physical pixel.
    float aa = aa_factor(in.local);
    float fill_cov = clamp(-d * aa, 0.0, 1.0);

    // Fill, premultiplied.
    float fa = in.color.a * fill_cov;
    float4 src = float4(in.color.rgb * fa, fa);

    // Border over fill (both premultiplied source-over).
    if (in.border_width > 0.0) {
        float bcov = clamp(-(abs(d) - in.border_width * 0.5) * aa, 0.0, 1.0);
        if (bcov > 0.0) {
            float ba = in.border_color.a * bcov;
            float4 bsrc = float4(in.border_color.rgb * ba, ba);
            src = bsrc + src * (1.0 - ba);
        }
    }
    return src;
}
"##;

/// The AnalyticLine MSL, frozen as a codegen oracle.
///
/// Like the other analytic families, this has no pre-migration hand-written
/// source: it was introduced directly on the typed IR, so its oracle is the
/// codegen output itself, locked by the byte-equivalence self-consistency test.
/// It still needs a one-time real-Metal compile to trust (the headless backend
/// does not compile MSL; see `viso-msl-reserved-half`).
pub const ANALYTIC_LINE_MSL_ORIGINAL: &str = r##"
#include <metal_stdlib>
using namespace metal;

struct InstanceIn {
    packed_float2 p0;
    packed_float2 p1;
    float width;
    packed_float4 color;
    uint cap;
    uint join;
    float miter_limit;
    float border_width;
    packed_float4 border_color;
};

struct Uniforms {
    packed_float2 viewport;
};

struct VOut {
    float4 position [[position]];
    float2 local;        // pixel-space sample position
    float2 seg_a;        // segment start (pixels)
    float2 seg_b;        // segment end (pixels)
    float half_width;    // stroke half-width (pixels)
    uint cap [[flat]];            // 0=butt 1=square 2=round
    uint join [[flat]];           // 0=miter 1=bevel 2=round
    float miter_limit;
    float border_width;
    float4 color;
    float4 border_color;
};

vertex VOut vertex_main(uint vid [[vertex_id]],
                        uint iid [[instance_id]],
                        const device InstanceIn* instances [[buffer(1)]],
                        constant Uniforms& u [[buffer(0)]]) {
    InstanceIn inst = instances[iid];

    // A line has no axis-aligned rect: derive a bounding quad rotated along the
    // segment direction. `t` runs 0→1 along the segment, `s` runs -1→+1 across it.
    // Two triangles: (0,-1)(1,-1)(0,+1) and (1,-1)(1,+1)(0,+1).
    float2 ts;
    switch (vid) {
        case 0: ts = float2(0.0, -1.0); break;
        case 1: ts = float2(1.0, -1.0); break;
        case 2: ts = float2(0.0,  1.0); break;
        case 3: ts = float2(1.0, -1.0); break;
        case 4: ts = float2(1.0,  1.0); break;
        default: ts = float2(0.0, 1.0); break;
    }

    float2 p0 = float2(inst.p0);
    float2 p1 = float2(inst.p1);
    float hw = inst.width * 0.5;

    // Segment direction and normal. A degenerate (zero-length) segment falls back
    // to +x so the quad stays well-formed and the SDF still renders the caps.
    float2 delta = p1 - p0;
    float len = length(delta);
    float2 dir = len > 0.0 ? delta / len : float2(1.0, 0.0);
    float2 nrm = float2(-dir.y, dir.x);

    // Square/round caps extend the geometry by a half-width past each end; butt
    // caps do not. Pad by 1px each side (along and across) for the AA ramp.
    float cap_ext = inst.cap == 0u ? 0.0 : hw;
    float pad = 1.0;
    float2 endpoint = p0 + dir * (ts.x * len);
    float along = ts.x < 0.5 ? -(cap_ext + pad) : (cap_ext + pad);
    float2 pixel = endpoint + dir * along + nrm * (ts.y * (hw + pad));

    // Pixel-space (top-left origin) → NDC. Y is flipped for Metal.
    float2 vp = float2(u.viewport);
    float2 ndc = float2(pixel.x / vp.x * 2.0 - 1.0,
                        1.0 - pixel.y / vp.y * 2.0);

    VOut out;
    out.position = float4(ndc, 0.0, 1.0);
    out.local = pixel;
    out.seg_a = p0;
    out.seg_b = p1;
    out.half_width = hw;
    out.cap = inst.cap;
    out.join = inst.join;
    out.miter_limit = inst.miter_limit;
    out.color = float4(inst.color);
    out.border_width = inst.border_width;
    out.border_color = float4(inst.border_color);
    return out;
}

// Signed distance to a line segment of half-width `hw` (IQ): project the sample
// onto the segment (clamped to its endpoints), then take the distance to that
// nearest point minus the half-width. Negative inside. This alone yields round
// caps (the endpoint projection rounds naturally); butt/square caps are applied
// by the fragment as an along-axis trim/extension of the clamp parameter.
static inline float segment_sdf(float2 p, float2 a, float2 b, float hw) {
    float2 pa = p - a;
    float2 ba = b - a;
    float denom = dot(ba, ba);
    float h = denom > 0.0 ? clamp(dot(pa, ba) / denom, 0.0, 1.0) : 0.0;
    return length(pa - ba * h) - hw;
}

// Signed distance for a butt or square cap. `cap_ext` extends the segment span
// by a half-width past each end (square); 0 keeps it flush (butt). The end faces
// are half-planes perpendicular to the segment, intersected with the round-cap
// body so the sides stay straight — a max() of the segment SDF and the two
// end-plane distances.
static inline float capped_segment_sdf(float2 p, float2 a, float2 b, float hw, float cap_ext) {
    float2 ba = b - a;
    float len = length(ba);
    float2 dir = len > 0.0 ? ba / len : float2(1.0, 0.0);
    float t = dot(p - a, dir);
    float d = segment_sdf(p, a, b, hw);
    // Trim past the (possibly extended) ends with perpendicular half-planes.
    float end_d = max(-(t + cap_ext), t - (len + cap_ext));
    return max(d, end_d);
}

// Device-pixel coverage factor: how many SDF units span one screen pixel at the
// current sampling position, inverted. Coverage ramps over ~1 device pixel
// regardless of scale, so the AA width tracks the physical grid.
static inline float aa_factor(float2 p) {
    return 1.0 / length(float2(length(dfdx(p)), length(dfdy(p))));
}

fragment float4 fragment_main(VOut in [[stage_in]]) {
    // Round cap (cap==2) is the bare segment SDF; butt (0) and square (1) trim or
    // extend the ends with perpendicular half-planes (cap_ext = 0 or half-width).
    float d;
    if (in.cap == 2u) {
        d = segment_sdf(in.local, in.seg_a, in.seg_b, in.half_width);
    } else {
        float cap_ext = in.cap == 1u ? in.half_width : 0.0;
        d = capped_segment_sdf(in.local, in.seg_a, in.seg_b, in.half_width, cap_ext);
    }

    // Device-pixel-aware coverage: linear ramp over ~1 physical pixel.
    float aa = aa_factor(in.local);
    float fill_cov = clamp(-d * aa, 0.0, 1.0);

    // Fill, premultiplied.
    float fa = in.color.a * fill_cov;
    float4 src = float4(in.color.rgb * fa, fa);

    // Border over fill (both premultiplied source-over).
    if (in.border_width > 0.0) {
        float bcov = clamp(-(abs(d) - in.border_width * 0.5) * aa, 0.0, 1.0);
        if (bcov > 0.0) {
            float ba = in.border_color.a * bcov;
            float4 bsrc = float4(in.border_color.rgb * ba, ba);
            src = bsrc + src * (1.0 - ba);
        }
    }
    return src;
}
"##;

/// The Gradient MSL, frozen as a codegen oracle.
///
/// Unlike the other literals here, the Gradient built-in never had hand-written
/// text: it is a new primitive whose source of truth is `gradient_ir`
/// (crate::ir::module). This literal is the *first* emitted `emit_msl` output,
/// captured verbatim so the byte-equivalence test pins the codegen against a fixed
/// reference. A real Metal device has not yet compiled it (the headless backend
/// does not compile MSL; see `viso-msl-reserved-half`).
pub const GRADIENT_MSL_ORIGINAL: &str = r##"
#include <metal_stdlib>
using namespace metal;

struct InstanceIn {
    packed_float2 rect_pos;
    packed_float2 rect_size;
    uint kind;
    uint extend;
    packed_float2 p0;
    packed_float2 p1;
    float lut_v;
    uint use_lut;
    packed_float4 color0;
    packed_float4 color1;
};

struct Uniforms {
    packed_float2 viewport;
};

struct VOut {
    float4 position [[position]];
    float2 local;       // pixel-space sample position
    float2 rect_min;    // rect top-left (pixels)
    float2 rect_max;    // rect bottom-right (pixels)
    uint kind [[flat]];          // 0=linear 1=radial 2=sweep
    uint extend [[flat]];        // 0=clamp 1=repeat 2=mirror
    float2 g0;          // gradient p0 (pixels)
    float2 g1;          // gradient p1 / (radius,_) / (angle,_)
    float lut_v;        // LUT row for this gradient
    uint use_lut [[flat]];       // 0=inline 2-stop 1=LUT
    float4 color0;      // inline stop 0 (premultiplied)
    float4 color1;      // inline stop 1 (premultiplied)
};

vertex VOut vertex_main(uint vid [[vertex_id]],
                        uint iid [[instance_id]],
                        const device InstanceIn* instances [[buffer(1)]],
                        constant Uniforms& u [[buffer(0)]]) {
    InstanceIn inst = instances[iid];

    // Axis-aligned corner quad, two triangles: (0,0)(1,0)(0,1) and (1,0)(1,1)(0,1).
    // Pad by 1px each side so the AA ramp at the rect edge is covered.
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
    out.rect_min = pos;
    out.rect_max = pos + size;
    out.kind = inst.kind;
    out.extend = inst.extend;
    out.g0 = float2(inst.p0);
    out.g1 = float2(inst.p1);
    out.lut_v = inst.lut_v;
    out.use_lut = inst.use_lut;
    out.color0 = float4(inst.color0);
    out.color1 = float4(inst.color1);
    return out;
}

// The raw gradient parameter for each geometry, before extend wrapping.
// linear: project the sample onto the p0→p1 axis, normalized to [0,1] at the
// endpoints. radial: distance from the center p0 over the radius (g1.x). sweep:
// the angle around p0, offset by the start angle (g1.x) and normalized to a
// single [0,1] turn.
static inline float gradient_t(uint kind, float2 p, float2 g0, float2 g1) {
    if (kind == 1u) {
        float r = max(g1.x, 1e-6);
        return length(p - g0) / r;
    }
    if (kind == 2u) {
        float ang = atan2(p.y - g0.y, p.x - g0.x) - g1.x;
        float turn = ang * (1.0 / (2.0 * M_PI_F));
        return turn - floor(turn);
    }
    float2 axis = g1 - g0;
    float len2 = max(dot(axis, axis), 1e-12);
    return dot(p - g0, axis) / len2;
}

// Apply the extend mode to a raw parameter, yielding a [0,1] lookup coordinate.
// 0=clamp, 1=repeat (fract), 2=mirror (triangle wave over period 2).
static inline float gradient_extend(uint mode, float t) {
    if (mode == 1u) {
        return t - floor(t);
    }
    if (mode == 2u) {
        float u = t - 2.0 * floor(t * 0.5);
        return u > 1.0 ? 2.0 - u : u;
    }
    return clamp(t, 0.0, 1.0);
}

// Device-pixel coverage factor: how many SDF units span one screen pixel at the
// current sampling position, inverted. Coverage ramps over ~1 device pixel
// regardless of scale, so the AA width tracks the physical grid.
static inline float aa_factor(float2 p) {
    return 1.0 / length(float2(length(dfdx(p)), length(dfdy(p))));
}

fragment float4 fragment_main(VOut in [[stage_in]],
                              texture2d<float> tex [[texture(0)]],
                              sampler samp [[sampler(0)]]) {
    // Rectangle coverage: signed distance to the axis-aligned box (negative inside),
    // ramped over ~1 device pixel for AA on the edges.
    float2 center = (in.rect_min + in.rect_max) * 0.5;
    float2 half_ext = (in.rect_max - in.rect_min) * 0.5;
    float2 q = abs(in.local - center) - half_ext;
    float d = length(max(q, float2(0.0))) + min(max(q.x, q.y), 0.0);
    float aa = aa_factor(in.local);
    float cov = clamp(-d * aa, 0.0, 1.0);

    // Gradient color at this sample: raw parameter → extend wrap → LUT sample or
    // inline 2-stop lerp. Both the LUT texel and the inline colors are premultiplied
    // linear, so the resolved color is already premultiplied.
    float t = gradient_t(in.kind, in.local, in.g0, in.g1);
    float u = gradient_extend(in.extend, t);
    float4 grad = in.use_lut != 0u
        ? tex.sample(samp, float2(u, in.lut_v))
        : mix(in.color0, in.color1, u);

    // Modulate the premultiplied gradient color by the edge coverage.
    return grad * cov;
}
"##;

/// The AnalyticShadow MSL, frozen as a codegen oracle. Same provenance as
/// [`ANALYTIC_CAPSULE_MSL_ORIGINAL`]: a new family with no historical hand-
/// written Metal, so the oracle is the codegen output itself, locked by the
/// byte-equivalence self-check in `codegen_msl`.
pub const ANALYTIC_SHADOW_MSL_ORIGINAL: &str = r##"
#include <metal_stdlib>
using namespace metal;

struct InstanceIn {
    packed_float2 rect_pos;
    packed_float2 rect_size;
    packed_float4 color;
    packed_float4 radius;
    packed_float2 offset;
    float sigma;
    float spread;
    uint shape;
};

struct Uniforms {
    packed_float2 viewport;
};

struct VOut {
    float4 position [[position]];
    float2 local;        // pixel-space position relative to the padded rect
    float2 half_size;    // half extents of the source rect (pixels)
    float2 center;       // source rect center (pixels)
    float4 radius;       // per-corner radius: lt, rt, rb, lb
    float2 offset;       // shadow offset (pixels)
    float sigma;         // blur standard deviation (pixels)
    float spread;        // silhouette grow/shrink (pixels)
    uint shape [[flat]];          // 0=rounded box 1=ellipse 2=capsule
    float4 color;
};

vertex VOut vertex_main(uint vid [[vertex_id]],
                        uint iid [[instance_id]],
                        const device InstanceIn* instances [[buffer(1)]],
                        constant Uniforms& u [[buffer(0)]]) {
    InstanceIn inst = instances[iid];

    // Two triangles over the padded quad. The pad must enclose the whole soft
    // footprint: the blur reaches ~3 sigma past the edge, spread grows the
    // silhouette, and the offset slides it — so pad = 3*sigma + spread + |offset|
    // on each axis (plus 1px for the AA fallback of a spread-only shadow).
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
    float reach = 3.0 * inst.sigma + max(inst.spread, 0.0) + 1.0;
    float2 pad = float2(reach, reach) + abs(float2(inst.offset));
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
    out.radius = float4(inst.radius);
    out.offset = float2(inst.offset);
    out.sigma = inst.sigma;
    out.spread = inst.spread;
    out.shape = inst.shape;
    out.color = float4(inst.color);
    return out;
}

// Signed distance to a rounded box, negative inside. `radii` is per corner
// (left-top, right-top, right-bottom, left-bottom); the sample's quadrant picks
// the active radius, clamped so it never exceeds the smaller full extent.
// `half_ext` is the box half-extents. (Never name it `half` — that is the
// reserved MSL 16-bit float type.)
static inline float shadow_rrect_sdf(float2 p, float2 half_ext, float4 radii) {
    float r = p.x < 0.0 ? (p.y < 0.0 ? radii.x : radii.w)
                        : (p.y < 0.0 ? radii.y : radii.z);
    float k = min(2.0 * r, min(half_ext.x, half_ext.y));
    float2 q = abs(p) - (half_ext - k);
    float2 mx = max(q, float2(0.0));
    return length(mx) + min(max(q.x, q.y), 0.0) - k;
}

// Signed distance to an axis-aligned ellipse inscribed in the box, negative
// inside: normalize the sample by the per-axis radii, offset by the unit
// circle, then scale back by the smaller radius. Matches the analytic-ellipse
// family's `ellipse_sdf`.
static inline float shadow_ellipse_sdf(float2 p, float2 half_ext) {
    float2 r = max(half_ext, float2(1e-4));
    float2 n = p / r;
    return (length(n) - 1.0) * min(r.x, r.y);
}

// Signed distance to a horizontal or vertical capsule (stadium): a rounded box
// whose corner radius equals the smaller half-extent, so the short axis is a
// pair of semicircle caps.
static inline float shadow_capsule_sdf(float2 p, float2 half_ext) {
    float r = min(half_ext.x, half_ext.y);
    float2 q = abs(p) - (half_ext - float2(r));
    float2 mx = max(q, float2(0.0));
    return length(mx) + min(max(q.x, q.y), 0.0) - r;
}

// Selected shadow silhouette SDF at a rect-centered sample. 0=rounded box,
// 1=ellipse, 2=capsule.
static inline float shadow_sdf(uint shape, float2 p, float2 half_ext, float4 radii) {
    if (shape == 1u) { return shadow_ellipse_sdf(p, half_ext); }
    if (shape == 2u) { return shadow_capsule_sdf(p, half_ext); }
    return shadow_rrect_sdf(p, half_ext, radii);
}

// Abramowitz & Stegun 7.1.26 error-function approximation (|error| < 1.5e-7).
static inline float erf_approx(float x) {
    float s = sign(x);
    float ax = abs(x);
    float t = 1.0 / (1.0 + 0.3275911 * ax);
    float y = 1.0 - (((((1.061405429 * t - 1.453152027) * t) + 1.421413741) * t
                    - 0.284496736) * t + 0.254829592) * t * exp(-ax * ax);
    return s * y;
}

// Device-pixel coverage factor: SDF units per screen pixel, inverted, so the
// sharp fallback ramp spans ~1 physical pixel at any scale.
static inline float shadow_aa(float2 p) {
    return 1.0 / length(float2(length(dfdx(p)), length(dfdy(p))));
}

fragment float4 fragment_main(VOut in [[stage_in]]) {
    // Sample in rect-centered space, shifted opposite the shadow offset so the
    // silhouette lands at +offset on screen. Spread grows (or shrinks) the
    // silhouette by insetting the half-extents.
    float2 p = in.local - in.center - in.offset;
    float2 half_ext = max(in.half_size + float2(in.spread), float2(0.0));
    float d = shadow_sdf(in.shape, p, half_ext, in.radius);

    // Soft coverage: model the blurred edge as a 1-D Gaussian applied to the signed
    // distance, so coverage = 1 - Phi(d/sigma) = 0.5*(1 - erf(d/(sqrt2*sigma))).
    // A near-zero sigma has no blur, so fall back to the device-pixel AA ramp.
    float cov;
    if (in.sigma > 0.01) {
        cov = 0.5 * (1.0 - erf_approx(d / (1.4142135 * in.sigma)));
    } else {
        cov = clamp(-d * shadow_aa(in.local), 0.0, 1.0);
    }
    cov = clamp(cov, 0.0, 1.0);

    // Premultiplied source-over.
    float fa = in.color.a * cov;
    return float4(in.color.rgb * fa, fa);
}
"##;
