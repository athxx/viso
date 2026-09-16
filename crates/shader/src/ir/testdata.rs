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
