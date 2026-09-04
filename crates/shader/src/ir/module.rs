//! One typed shader IR value describing a built-in primitive's GPU interface —
//! the single source of truth that emits *both* the MSL `InstanceIn`/`VertexIn`
//! struct *and* the [`InstanceSchema`](viso_gpu::InstanceSchema) the pipeline validates against
//! (architecture section 36 / AGENTS 19).
//!
//! ## What the IR owns, and what it carries
//!
//! The load-bearing win of this slice is collapsing a duplicated field contract:
//! before, each built-in spelled its per-instance fields twice — once as a
//! `packed_float*` MSL `struct` and once as a parallel `SchemaAttr` list — kept in
//! lockstep only by name-order test assertions, with byte offsets never
//! cross-checked (the section-56 "implicit shader instance field-order ABI").
//! Here a [`ShaderIr`]'s [`attributes`](ShaderIr::attributes) field list is the
//! *only* place that contract is written; the MSL struct, the schema, and the
//! section-36.1 offset expectations all project from it, so they cannot drift.
//!
//! The IR is strongly typed at the **interface** layer — instance/uniform/varying
//! fields, texture/sampler bindings, and buffer indices are typed data. The
//! per-primitive vertex/fragment *algorithm* body is carried as a structured MSL
//! fragment (the exact math the hand-written built-ins already shipped), not an
//! expression AST: building an expression-level shader AST is only needed once
//! users write shader *logic* rather than instantiating built-ins, and that is
//! explicitly out of scope this slice (it would prematurely duplicate the
//! `viso-dsl` frontend). The pipeline shape `source → parsed syntax → typed IR →
//! validation → codegen` is satisfied with a Rust-side structured IR builder as
//! the "parsed syntax → typed IR" front, which is the right front for built-ins.
//!
//! This takes makepad's *semantics* (its packing rule, its per-primitive math)
//! and rebuilds them as a real typed IR without sharing a script VM — the
//! `viso-diverge-from-makepad` divergence AGENTS 19 mandates.

use viso_gpu::SchemaAttr;

use crate::PrimitiveKind;
use crate::ir::types::IrType;

/// One typed field of a shader interface `struct` (an instance/vertex attribute,
/// a uniform member, or a varying). The `name` and `ty` are the whole contract;
/// the MSL spelling and the schema/offset projection are derived from `ty`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct IrField {
    /// The field name, shared verbatim by the MSL struct member and the
    /// [`SchemaAttr`] it projects to.
    pub name: &'static str,
    /// The field type — the single input to both the MSL spelling and the
    /// [`AttrFormat`](viso_gpu::AttrFormat) projection.
    pub ty: IrType,
}

impl IrField {
    /// Convenience constructor for a `const` field list.
    pub const fn new(name: &'static str, ty: IrType) -> Self {
        IrField { name, ty }
    }
}

/// Which per-vertex data source a primitive's vertex stage reads. Built-ins split
/// into two shapes, and the shape decides the MSL entry-point signature and the
/// buffer indices the backend binds against.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VertexSource {
    /// The vertex shader synthesizes geometry from `vertex_id`/`instance_id` and
    /// reads a per-*instance* buffer (Quad/Image/GlyphRun). The instance buffer is
    /// at Metal buffer index 1 and the uniforms at index 0.
    PerInstance,
    /// The vertex shader reads a real per-*vertex* buffer indexed by `vertex_id`
    /// (Path/Mesh); there is no instance buffer, so the vertex buffer takes index
    /// 0 and the uniforms move to index 1.
    PerVertex,
}

impl VertexSource {
    /// The Metal buffer index the attribute buffer (`InstanceIn`/`VertexIn`) binds
    /// to. Per-instance data sits at 1 (uniforms own 0); a per-vertex buffer sits
    /// at 0 (uniforms move to 1).
    pub const fn attr_buffer_index(self) -> u32 {
        match self {
            VertexSource::PerInstance => 1,
            VertexSource::PerVertex => 0,
        }
    }

    /// The Metal buffer index the inline `Uniforms` bind to — the complement of
    /// [`attr_buffer_index`](Self::attr_buffer_index).
    pub const fn uniform_buffer_index(self) -> u32 {
        match self {
            VertexSource::PerInstance => 0,
            VertexSource::PerVertex => 1,
        }
    }

    /// The MSL name of the attribute `struct` this source declares.
    pub const fn attr_struct_name(self) -> &'static str {
        match self {
            VertexSource::PerInstance => "InstanceIn",
            VertexSource::PerVertex => "VertexIn",
        }
    }
}

/// The typed IR for one built-in primitive shader: a strongly-typed interface
/// (attributes, uniforms, varyings, bindings) plus the verbatim MSL body math the
/// codegen splices between the generated declarations.
///
/// A `ShaderIr` value is constructed by one of [`quad_ir`]/[`image_ir`]/
/// [`glyphrun_ir`]/[`mesh_ir`] — the only hand-written per-primitive field
/// contracts in the crate.
#[derive(Debug, Clone)]
pub struct ShaderIr {
    /// Which built-in this describes.
    pub kind: PrimitiveKind,
    /// How the vertex stage sources data (decides the entry-point signature and
    /// buffer indices).
    pub vertex_source: VertexSource,
    /// The per-instance (or per-vertex) attribute fields — the single source of
    /// truth for the MSL attribute `struct` *and* the [`InstanceSchema`](viso_gpu::InstanceSchema).
    pub attributes: &'static [IrField],
    /// The inline `Uniforms` members (viewport, etc.).
    pub uniforms: &'static [IrField],
    /// The `VOut` varying members carried vertex → fragment. Each carries a
    /// trailing verbatim MSL attribute string (e.g. `" [[position]]"`) and an
    /// optional line comment, since varyings are a codegen detail, not part of
    /// the validated instance ABI.
    pub varyings: &'static [Varying],
    /// The number of textures bound at `[[texture(0..)]]` (0 for Quad/Mesh, 1 for
    /// Image/GlyphRun). Samplers track textures 1:1.
    pub texture_count: u32,
    /// The verbatim body of `vertex_main` — the statements between the opening
    /// `{` and the closing `}`, exactly as the hand-written built-in shipped.
    pub vertex_body: &'static str,
    /// Free-standing helper functions emitted between the vertex and fragment
    /// entry points (e.g. Quad's `box_sdf`), verbatim.
    pub helpers: &'static str,
    /// The verbatim body of `fragment_main`.
    pub fragment_body: &'static str,
}

/// A `VOut` varying: a typed field plus its verbatim MSL attribute suffix and an
/// optional trailing line comment (both codegen-only presentation, not ABI).
#[derive(Debug, Clone, Copy)]
pub struct Varying {
    /// The varying field name.
    pub name: &'static str,
    /// The varying value type (unpacked `floatN` in the `VOut` struct).
    pub ty: IrType,
    /// A verbatim MSL attribute suffix printed after the field, e.g.
    /// `" [[position]]"`; empty for a plain interpolated varying.
    pub attr: &'static str,
    /// An optional trailing line comment (without the `//`), or empty.
    pub comment: &'static str,
}

impl Varying {
    const fn new(
        name: &'static str,
        ty: IrType,
        attr: &'static str,
        comment: &'static str,
    ) -> Self {
        Varying {
            name,
            ty,
            attr,
            comment,
        }
    }
}

impl ShaderIr {
    /// The schema attributes as an owned vector — the projection of each attribute
    /// field's type to its [`SchemaAttr`]. This is the *same* field list the MSL
    /// attribute struct is generated from, so the schema and the struct are one
    /// source and cannot drift.
    ///
    /// `msl.rs` caches this behind a per-primitive `OnceLock` and hands out a
    /// `&'static [SchemaAttr]` to satisfy [`InstanceSchema`](viso_gpu::InstanceSchema)'s `'static` borrow —
    /// a one-time, cold-path (pipeline-registration) materialization with zero
    /// steady-state cost.
    pub fn schema_attrs(&self) -> Vec<SchemaAttr> {
        self.attributes
            .iter()
            .map(|f| SchemaAttr {
                name: f.name,
                format: f.ty.to_attr_format(),
            })
            .collect()
    }

    /// The expected byte offset of each attribute field in a tightly-packed
    /// `#[repr(C)]` layout, as `(name, offset)` pairs, plus the total packed
    /// stride. This is the section-36.1 expectation the GPU-side
    /// `validate_against` cross-checks against the CPU `offset_of!` truth.
    ///
    /// Every allowed field is 4-byte-aligned, so the prefix sum of packed sizes is
    /// the real `#[repr(C)]` offset with no padding — which is exactly why the
    /// built-in structs need no inter-field padding.
    pub fn expected_offsets(&self) -> (Vec<(&'static str, usize)>, usize) {
        let mut offset = 0usize;
        let mut out = Vec::with_capacity(self.attributes.len());
        for f in self.attributes {
            let align = f.ty.align();
            // Round up to the field alignment (a no-op here since everything is
            // 4-byte-aligned, but written explicitly so the invariant is visible).
            offset = offset.div_ceil(align) * align;
            out.push((f.name, offset));
            offset += f.ty.packed_size();
        }
        (out, offset)
    }
}

// The four built-in field contracts. These constructors are the *only* place a
// built-in primitive's per-instance/per-vertex field list, uniform members,
// varyings, and body math are written. Everything downstream (the MSL struct, the
// schema, the offset expectations) projects from them.

/// Rounded/bordered-rectangle built-in. Per-instance data, uniforms at buffer 0.
///
/// Fields, varyings and body math are byte-for-byte the historical hand-written
/// `QUAD_MSL`; codegen re-emits them so the migration to an IR source is a no-op
/// on the produced Metal text.
pub fn quad_ir() -> ShaderIr {
    static ATTRS: &[IrField] = &[
        IrField::new("rect_pos", IrType::F32X2),
        IrField::new("rect_size", IrType::F32X2),
        IrField::new("color", IrType::F32X4),
        IrField::new("radius", IrType::F32),
        IrField::new("border_width", IrType::F32),
        IrField::new("border_color", IrType::F32X4),
    ];
    static UNIFORMS: &[IrField] = &[IrField::new("viewport", IrType::F32X2)];
    static VARYINGS: &[Varying] = &[
        Varying::new("position", IrType::F32X4, " [[position]]", ""),
        Varying::new(
            "local",
            IrType::F32X2,
            "",
            "pixel-space position relative to the padded rect",
        ),
        Varying::new(
            "half_size",
            IrType::F32X2,
            "",
            "half extents of the rect (pixels)",
        ),
        Varying::new("center", IrType::F32X2, "", "rect center (pixels)"),
        Varying::new("radius", IrType::F32, "", ""),
        Varying::new("border_width", IrType::F32, "", ""),
        Varying::new("color", IrType::F32X4, "", ""),
        Varying::new("border_color", IrType::F32X4, "", ""),
    ];
    ShaderIr {
        kind: PrimitiveKind::Quad,
        vertex_source: VertexSource::PerInstance,
        attributes: ATTRS,
        uniforms: UNIFORMS,
        varyings: VARYINGS,
        texture_count: 0,
        vertex_body: QUAD_VERTEX_BODY,
        helpers: QUAD_HELPERS,
        fragment_body: QUAD_FRAGMENT_BODY,
    }
}

/// Textured-image built-in. Per-instance data, uniforms at buffer 0, one texture.
pub fn image_ir() -> ShaderIr {
    static ATTRS: &[IrField] = &[
        IrField::new("rect_pos", IrType::F32X2),
        IrField::new("rect_size", IrType::F32X2),
        IrField::new("uv_pos", IrType::F32X2),
        IrField::new("uv_size", IrType::F32X2),
        IrField::new("color", IrType::F32X4),
    ];
    static UNIFORMS: &[IrField] = &[IrField::new("viewport", IrType::F32X2)];
    static VARYINGS: &[Varying] = &[
        Varying::new("position", IrType::F32X4, " [[position]]", ""),
        Varying::new("uv", IrType::F32X2, "", ""),
        Varying::new("tint", IrType::F32X4, "", ""),
    ];
    ShaderIr {
        kind: PrimitiveKind::Image,
        vertex_source: VertexSource::PerInstance,
        attributes: ATTRS,
        uniforms: UNIFORMS,
        varyings: VARYINGS,
        texture_count: 1,
        vertex_body: IMAGE_VERTEX_BODY,
        helpers: "",
        fragment_body: IMAGE_FRAGMENT_BODY,
    }
}

/// Glyph-run built-in: the image contract plus a per-instance `px_range` the
/// fragment uses to turn the sampled SDF back into coverage.
pub fn glyphrun_ir() -> ShaderIr {
    static ATTRS: &[IrField] = &[
        IrField::new("rect_pos", IrType::F32X2),
        IrField::new("rect_size", IrType::F32X2),
        IrField::new("uv_pos", IrType::F32X2),
        IrField::new("uv_size", IrType::F32X2),
        IrField::new("color", IrType::F32X4),
        IrField::new("px_range", IrType::F32),
    ];
    static UNIFORMS: &[IrField] = &[IrField::new("viewport", IrType::F32X2)];
    static VARYINGS: &[Varying] = &[
        Varying::new("position", IrType::F32X4, " [[position]]", ""),
        Varying::new("uv", IrType::F32X2, "", ""),
        Varying::new("color", IrType::F32X4, "", ""),
        Varying::new("px_range", IrType::F32, "", ""),
    ];
    ShaderIr {
        kind: PrimitiveKind::GlyphRun,
        vertex_source: VertexSource::PerInstance,
        attributes: ATTRS,
        uniforms: UNIFORMS,
        varyings: VARYINGS,
        texture_count: 1,
        vertex_body: GLYPHRUN_VERTEX_BODY,
        helpers: "",
        fragment_body: GLYPHRUN_FRAGMENT_BODY,
    }
}

/// General mesh built-in (shared by Path and Mesh): a real per-vertex buffer at
/// index 0, uniforms at index 1, no instance buffer, no texture.
pub fn mesh_ir() -> ShaderIr {
    static ATTRS: &[IrField] = &[
        IrField::new("pos", IrType::F32X2),
        IrField::new("color", IrType::F32X4),
        IrField::new("edge", IrType::F32),
    ];
    static UNIFORMS: &[IrField] = &[IrField::new("viewport", IrType::F32X2)];
    static VARYINGS: &[Varying] = &[
        Varying::new("position", IrType::F32X4, " [[position]]", ""),
        Varying::new("color", IrType::F32X4, "", ""),
        Varying::new("edge", IrType::F32, "", ""),
    ];
    ShaderIr {
        kind: PrimitiveKind::Mesh,
        vertex_source: VertexSource::PerVertex,
        attributes: ATTRS,
        uniforms: UNIFORMS,
        varyings: VARYINGS,
        texture_count: 0,
        vertex_body: MESH_VERTEX_BODY,
        helpers: "",
        fragment_body: MESH_FRAGMENT_BODY,
    }
}

// The verbatim per-primitive body math. Each string is the exact statement block
// between `vertex_main`/`fragment_main`'s braces (or, for `helpers`, a run of
// free-standing functions) as the hand-written built-in shipped it, so the
// generated declarations plus these fragments reproduce the historical MSL
// byte-for-byte. Codegen indents each non-empty line by four spaces and frames it
// with the generated signature, so the fragments are stored *un*indented and
// without their braces.

const QUAD_VERTEX_BODY: &str = "\
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
return out;";

const QUAD_HELPERS: &str = "\
// Signed distance to a rounded box (IQ), negative inside. `k` is the doubled,
// clamped corner radius.
// `half_ext` is the box's half-extents. (Do not name it `half` — that is a
// reserved MSL type name, the 16-bit float.)
static inline float box_sdf(float2 p, float2 center, float2 half_ext, float k) {
    float2 q = abs(p - center) - (half_ext - k);
    float2 mx = max(q, float2(0.0));
    return length(mx) + min(max(q.x, q.y), 0.0) - k;
}";

const QUAD_FRAGMENT_BODY: &str = "\
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
return src;";

const IMAGE_VERTEX_BODY: &str = "\
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
return out;";

const IMAGE_FRAGMENT_BODY: &str = "\
// Texel is premultiplied linear (Viso texture convention). Scale it by the
// straight tint's premultiplied form: rgb by (tint.rgb * tint.a), a by
// tint.a — keeping the result premultiplied.
float4 texel = tex.sample(samp, in.uv);
float4 t = float4(in.tint.rgb * in.tint.a, in.tint.a);
return texel * t;";

const GLYPHRUN_VERTEX_BODY: &str = "\
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
return out;";

const GLYPHRUN_FRAGMENT_BODY: &str = "\
// Decode the ESDT single-channel SDF back to coverage. The glyph edge sits
// at stored 0.75 (= 1 - cutoff, cutoff fixed at 0.25 in the rasterizer);
// `px_range` stored-units span one screen pixel of the coverage ramp.
float sd = tex.sample(samp, in.uv).r;
float cov = clamp((sd - 0.75) * in.px_range + 0.5, 0.0, 1.0);
float a = in.color.a * cov;
return float4(in.color.rgb * a, a);";

const MESH_VERTEX_BODY: &str = "\
VertexIn v = verts[vid];

float2 pixel = float2(v.pos);
float2 vp = float2(u.viewport);
float2 ndc = float2(pixel.x / vp.x * 2.0 - 1.0,
                    1.0 - pixel.y / vp.y * 2.0);

VOut out;
out.position = float4(ndc, 0.0, 1.0);
out.color = float4(v.color);
out.edge = v.edge;
return out;";

const MESH_FRAGMENT_BODY: &str = "\
float cov = clamp(in.edge, 0.0, 1.0);
float a = in.color.a * cov;
return float4(in.color.rgb * a, a);";
