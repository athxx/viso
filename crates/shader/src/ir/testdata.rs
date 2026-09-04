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

/// The original hand-written GLYPHRUN MSL, frozen as a codegen oracle.
pub const GLYPHRUN_MSL_ORIGINAL: &str = r##"
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
