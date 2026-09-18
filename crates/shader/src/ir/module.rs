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
//! Packing rules and per-primitive math are represented as a typed IR rather
//! than interpreted through a shared script VM.

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

/// Glyph-run built-in: the image contract sampling a single-channel A8 coverage
/// atlas. The fragment reads the texel's coverage directly and modulates the
/// run color by it — no signed-distance decode.
pub fn glyphrun_ir() -> ShaderIr {
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
        Varying::new("color", IrType::F32X4, "", ""),
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

/// Analytic rounded-rectangle built-in: an axis-aligned rectangle with an
/// independent corner radius per corner (`radius[4]`, ordered left-top,
/// right-top, right-bottom, left-bottom) and an optional inner/outer
/// antialiased border. Per-instance data, uniforms at buffer 0.
///
/// The single scalar-radius Quad built-in cannot express per-corner radii, so
/// this is a distinct family with its own `packed_float4 radius` instance field.
pub fn analytic_rrect_ir() -> ShaderIr {
    static ATTRS: &[IrField] = &[
        IrField::new("rect_pos", IrType::F32X2),
        IrField::new("rect_size", IrType::F32X2),
        IrField::new("color", IrType::F32X4),
        IrField::new("radius", IrType::F32X4),
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
        Varying::new(
            "radius",
            IrType::F32X4,
            "",
            "per-corner radius: lt, rt, rb, lb",
        ),
        Varying::new("border_width", IrType::F32, "", ""),
        Varying::new("color", IrType::F32X4, "", ""),
        Varying::new("border_color", IrType::F32X4, "", ""),
    ];
    ShaderIr {
        kind: PrimitiveKind::AnalyticRRect,
        vertex_source: VertexSource::PerInstance,
        attributes: ATTRS,
        uniforms: UNIFORMS,
        varyings: VARYINGS,
        texture_count: 0,
        vertex_body: ANALYTIC_RRECT_VERTEX_BODY,
        helpers: ANALYTIC_RRECT_HELPERS,
        fragment_body: ANALYTIC_RRECT_FRAGMENT_BODY,
    }
}

/// Analytic shadow fast lane (architecture section 15.1): a soft drop shadow for
/// the analytic shape families — rounded box, ellipse/circle, capsule — drawn as
/// one expanded instance quad whose coverage is a closed-form Gaussian ramp over
/// the shape's signed distance. It is deliberately *not* the mask/blur/composite
/// path: a simple shadow allocates no offscreen layer, blurs no texture, and adds
/// one instanced draw under the shape.
///
/// The instance carries the source rect, a `radius` (`packed_float4`, per corner
/// for the rounded-box case), the shadow `color`, the `offset` in pixels, the blur
/// `sigma`, the `spread` (positive grows the silhouette, negative shrinks it), and
/// a `shape` selector (0 = rounded box, 1 = ellipse, 2 = capsule). The vertex
/// stage expands the quad by `3*sigma + spread + max(|offset|)` past the rect so
/// the whole blurred, offset, spread footprint is inside the draw. The fragment
/// insets the half-extents by `spread`, shifts the sample by `-offset`, evaluates
/// the selected SDF, and maps the distance through a Gaussian integral so the edge
/// softens over `~sigma`; a near-zero `sigma` falls back to the device-pixel AA
/// ramp so a spread-only shadow stays crisp.
pub fn analytic_shadow_ir() -> ShaderIr {
    static ATTRS: &[IrField] = &[
        IrField::new("rect_pos", IrType::F32X2),
        IrField::new("rect_size", IrType::F32X2),
        IrField::new("color", IrType::F32X4),
        IrField::new("radius", IrType::F32X4),
        IrField::new("offset", IrType::F32X2),
        IrField::new("sigma", IrType::F32),
        IrField::new("spread", IrType::F32),
        IrField::new("shape", IrType::U32),
        IrField::new("inner", IrType::U32),
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
            "half extents of the source rect (pixels)",
        ),
        Varying::new("center", IrType::F32X2, "", "source rect center (pixels)"),
        Varying::new(
            "radius",
            IrType::F32X4,
            "",
            "per-corner radius: lt, rt, rb, lb",
        ),
        Varying::new("offset", IrType::F32X2, "", "shadow offset (pixels)"),
        Varying::new("sigma", IrType::F32, "", "blur standard deviation (pixels)"),
        Varying::new("spread", IrType::F32, "", "silhouette grow/shrink (pixels)"),
        Varying::new(
            "shape",
            IrType::U32,
            " [[flat]]",
            "0=rounded box 1=ellipse 2=capsule",
        ),
        Varying::new(
            "inner",
            IrType::U32,
            " [[flat]]",
            "0=outer drop shadow 1=inner shadow",
        ),
        Varying::new("color", IrType::F32X4, "", ""),
    ];
    ShaderIr {
        kind: PrimitiveKind::AnalyticShadow,
        vertex_source: VertexSource::PerInstance,
        attributes: ATTRS,
        uniforms: UNIFORMS,
        varyings: VARYINGS,
        texture_count: 0,
        vertex_body: ANALYTIC_SHADOW_VERTEX_BODY,
        helpers: ANALYTIC_SHADOW_HELPERS,
        fragment_body: ANALYTIC_SHADOW_FRAGMENT_BODY,
    }
}

/// Analytic ellipse built-in: a scaled-circle SDF filling the rect's inscribed
/// ellipse (equal axes give a circle), with an optional inner/outer antialiased
/// border. Per-instance data, uniforms at buffer 0. The ellipse radii are the
/// rect's half-extents, so no radius instance field is needed.
pub fn analytic_ellipse_ir() -> ShaderIr {
    static ATTRS: &[IrField] = &[
        IrField::new("rect_pos", IrType::F32X2),
        IrField::new("rect_size", IrType::F32X2),
        IrField::new("color", IrType::F32X4),
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
            "ellipse radii = half extents (pixels)",
        ),
        Varying::new("center", IrType::F32X2, "", "ellipse center (pixels)"),
        Varying::new("border_width", IrType::F32, "", ""),
        Varying::new("color", IrType::F32X4, "", ""),
        Varying::new("border_color", IrType::F32X4, "", ""),
    ];
    ShaderIr {
        kind: PrimitiveKind::AnalyticEllipse,
        vertex_source: VertexSource::PerInstance,
        attributes: ATTRS,
        uniforms: UNIFORMS,
        varyings: VARYINGS,
        texture_count: 0,
        vertex_body: ANALYTIC_ELLIPSE_VERTEX_BODY,
        helpers: ANALYTIC_ELLIPSE_HELPERS,
        fragment_body: ANALYTIC_ELLIPSE_FRAGMENT_BODY,
    }
}

/// Analytic capsule (stadium) built-in: a rounded box whose corner radius is the
/// smaller half-extent, so the short axis is fully rounded into a semicircle and
/// the long axis is a straight run — a pill shape. Fill plus an optional
/// inner/outer antialiased border. Per-instance data, uniforms at buffer 0. The
/// capsule radius is derived in the shader (`min` of the half-extents), so no
/// radius instance field is needed.
pub fn analytic_capsule_ir() -> ShaderIr {
    static ATTRS: &[IrField] = &[
        IrField::new("rect_pos", IrType::F32X2),
        IrField::new("rect_size", IrType::F32X2),
        IrField::new("color", IrType::F32X4),
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
            "box half extents (pixels); capsule radius = min of the two",
        ),
        Varying::new("center", IrType::F32X2, "", "capsule center (pixels)"),
        Varying::new("border_width", IrType::F32, "", ""),
        Varying::new("color", IrType::F32X4, "", ""),
        Varying::new("border_color", IrType::F32X4, "", ""),
    ];
    ShaderIr {
        kind: PrimitiveKind::AnalyticCapsule,
        vertex_source: VertexSource::PerInstance,
        attributes: ATTRS,
        uniforms: UNIFORMS,
        varyings: VARYINGS,
        texture_count: 0,
        vertex_body: ANALYTIC_CAPSULE_VERTEX_BODY,
        helpers: ANALYTIC_CAPSULE_HELPERS,
        fragment_body: ANALYTIC_CAPSULE_FRAGMENT_BODY,
    }
}

/// Analytic line built-in: a stroked line segment defined by two endpoints
/// (`p0`/`p1`) and a center-aligned width, with per-end caps (butt/square/round),
/// a join style, a miter limit, and an optional inner/outer antialiased border.
/// Per-instance data, uniforms at buffer 0.
///
/// Unlike the other analytic families — which fill an axis-aligned rect's
/// inscribed shape — a line has no rect AABB: the vertex stage derives a bounding
/// quad *rotated* along the segment direction (`dir = normalize(p1 - p0)`, normal
/// `n`, half-width `hw`, plus a per-end cap extension and a 1px AA pad), so the
/// instance carries the endpoints, width, cap/join enums, and miter limit rather
/// than a rect. `cap`/`join` are scalar `u32` enums (a `packed_uint3` is never
/// produced; the type system maps u32 vectors only to Uint2/Uint4).
pub fn analytic_line_ir() -> ShaderIr {
    static ATTRS: &[IrField] = &[
        IrField::new("p0", IrType::F32X2),
        IrField::new("p1", IrType::F32X2),
        IrField::new("width", IrType::F32),
        IrField::new("color", IrType::F32X4),
        IrField::new("cap", IrType::U32),
        IrField::new("join", IrType::U32),
        IrField::new("miter_limit", IrType::F32),
        IrField::new("border_width", IrType::F32),
        IrField::new("border_color", IrType::F32X4),
    ];
    static UNIFORMS: &[IrField] = &[IrField::new("viewport", IrType::F32X2)];
    static VARYINGS: &[Varying] = &[
        Varying::new("position", IrType::F32X4, " [[position]]", ""),
        Varying::new("local", IrType::F32X2, "", "pixel-space sample position"),
        Varying::new("seg_a", IrType::F32X2, "", "segment start (pixels)"),
        Varying::new("seg_b", IrType::F32X2, "", "segment end (pixels)"),
        Varying::new("half_width", IrType::F32, "", "stroke half-width (pixels)"),
        Varying::new("cap", IrType::U32, " [[flat]]", "0=butt 1=square 2=round"),
        Varying::new("join", IrType::U32, " [[flat]]", "0=miter 1=bevel 2=round"),
        Varying::new("miter_limit", IrType::F32, "", ""),
        Varying::new("border_width", IrType::F32, "", ""),
        Varying::new("color", IrType::F32X4, "", ""),
        Varying::new("border_color", IrType::F32X4, "", ""),
    ];
    ShaderIr {
        kind: PrimitiveKind::AnalyticLine,
        vertex_source: VertexSource::PerInstance,
        attributes: ATTRS,
        uniforms: UNIFORMS,
        varyings: VARYINGS,
        texture_count: 0,
        vertex_body: ANALYTIC_LINE_VERTEX_BODY,
        helpers: ANALYTIC_LINE_HELPERS,
        fragment_body: ANALYTIC_LINE_FRAGMENT_BODY,
    }
}

/// Gradient built-in: an axis-aligned rect filled by a 1D gradient. Per-instance
/// data, uniforms at buffer 0, one texture (the 1D gradient LUT atlas at
/// `[[texture(0)]]`, sampled at row `lut_v`).
///
/// One family serves all three gradient geometries — the `kind` field selects
/// linear / radial / sweep in the fragment, so they share a pipeline. The stop
/// data is resolved off-instance: `use_lut != 0` samples the LUT, otherwise the
/// fast 2-stop path lerps the two inline **premultiplied** colors. `extend`
/// selects the clamp/repeat/mirror wrap of the gradient parameter `t`.
pub fn gradient_ir() -> ShaderIr {
    static ATTRS: &[IrField] = &[
        IrField::new("rect_pos", IrType::F32X2),
        IrField::new("rect_size", IrType::F32X2),
        IrField::new("kind", IrType::U32),
        IrField::new("extend", IrType::U32),
        IrField::new("p0", IrType::F32X2),
        IrField::new("p1", IrType::F32X2),
        IrField::new("lut_v", IrType::F32),
        IrField::new("use_lut", IrType::U32),
        IrField::new("color0", IrType::F32X4),
        IrField::new("color1", IrType::F32X4),
    ];
    static UNIFORMS: &[IrField] = &[IrField::new("viewport", IrType::F32X2)];
    static VARYINGS: &[Varying] = &[
        Varying::new("position", IrType::F32X4, " [[position]]", ""),
        Varying::new("local", IrType::F32X2, "", "pixel-space sample position"),
        Varying::new("rect_min", IrType::F32X2, "", "rect top-left (pixels)"),
        Varying::new("rect_max", IrType::F32X2, "", "rect bottom-right (pixels)"),
        Varying::new(
            "kind",
            IrType::U32,
            " [[flat]]",
            "0=linear 1=radial 2=sweep",
        ),
        Varying::new(
            "extend",
            IrType::U32,
            " [[flat]]",
            "0=clamp 1=repeat 2=mirror",
        ),
        Varying::new("g0", IrType::F32X2, "", "gradient p0 (pixels)"),
        Varying::new(
            "g1",
            IrType::F32X2,
            "",
            "gradient p1 / (radius,_) / (angle,_)",
        ),
        Varying::new("lut_v", IrType::F32, "", "LUT row for this gradient"),
        Varying::new("use_lut", IrType::U32, " [[flat]]", "0=inline 2-stop 1=LUT"),
        Varying::new("color0", IrType::F32X4, "", "inline stop 0 (premultiplied)"),
        Varying::new("color1", IrType::F32X4, "", "inline stop 1 (premultiplied)"),
    ];
    ShaderIr {
        kind: PrimitiveKind::Gradient,
        vertex_source: VertexSource::PerInstance,
        attributes: ATTRS,
        uniforms: UNIFORMS,
        varyings: VARYINGS,
        texture_count: 1,
        vertex_body: GRADIENT_VERTEX_BODY,
        helpers: GRADIENT_HELPERS,
        fragment_body: GRADIENT_FRAGMENT_BODY,
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
}

// Device-pixel coverage factor: how many SDF units span one screen pixel at the
// current sampling position, inverted. Coverage ramps over ~1 device pixel
// regardless of scale, so the AA width tracks the physical grid.
static inline float aa_factor(float2 p) {
    return 1.0 / length(float2(length(dfdx(p)), length(dfdy(p))));
}";

const QUAD_FRAGMENT_BODY: &str = "\
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
return src;";

const ANALYTIC_RRECT_VERTEX_BODY: &str = "\
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
return out;";

const ANALYTIC_RRECT_HELPERS: &str = "\
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
}";

const ANALYTIC_RRECT_FRAGMENT_BODY: &str = "\
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
return src;";

const ANALYTIC_SHADOW_VERTEX_BODY: &str = "\
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
out.inner = inst.inner;
out.color = float4(inst.color);
return out;";

const ANALYTIC_SHADOW_HELPERS: &str = "\
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
}";

const ANALYTIC_SHADOW_FRAGMENT_BODY: &str = "\
// Sample in rect-centered space, shifted opposite the shadow offset so the
// silhouette lands at +offset on screen. Spread grows (or shrinks) the
// silhouette by insetting the half-extents.
float2 p = in.local - in.center - in.offset;
float2 half_ext = max(in.half_size + float2(in.spread), float2(0.0));
float d = shadow_sdf(in.shape, p, half_ext, in.radius);

// Soft coverage: model the blurred edge as a 1-D Gaussian applied to the signed
// distance. An outer drop shadow fills the silhouette and fades outward
// (coverage = 1 - Phi(d/sigma) = 0.5*(1 - erf(d/(sqrt2*sigma)))). An inner shadow
// is its complement clipped to the interior: the blurred silhouette of the hole,
// darkest at the edge and fading toward the center — 0.5*(1 + erf(...)) times an
// inside mask. A near-zero sigma has no blur, so fall back to a device-pixel AA
// ramp (a signed hairline at the edge for the inner case, a filled ramp otherwise).
float cov;
if (in.inner != 0u) {
    if (in.sigma > 0.01) {
        float soft = 0.5 * (1.0 + erf_approx(d / (1.4142135 * in.sigma)));
        // Clip to the interior with the same Gaussian edge, so the darkening
        // lives inside the silhouette and vanishes at and beyond the edge.
        float inside = 0.5 * (1.0 - erf_approx(d / (1.4142135 * in.sigma)));
        cov = soft * inside;
    } else {
        cov = clamp(-d * shadow_aa(in.local), 0.0, 1.0);
    }
} else if (in.sigma > 0.01) {
    cov = 0.5 * (1.0 - erf_approx(d / (1.4142135 * in.sigma)));
} else {
    cov = clamp(-d * shadow_aa(in.local), 0.0, 1.0);
}
cov = clamp(cov, 0.0, 1.0);

// Premultiplied source-over.
float fa = in.color.a * cov;
return float4(in.color.rgb * fa, fa);";

const ANALYTIC_ELLIPSE_VERTEX_BODY: &str = "\
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
return out;";

const ANALYTIC_ELLIPSE_HELPERS: &str = "\
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
}";

const ANALYTIC_ELLIPSE_FRAGMENT_BODY: &str = "\
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
return src;";

const ANALYTIC_CAPSULE_VERTEX_BODY: &str = "\
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
return out;";

const ANALYTIC_CAPSULE_HELPERS: &str = "\
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
}";

const ANALYTIC_CAPSULE_FRAGMENT_BODY: &str = "\
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
return src;";

const ANALYTIC_LINE_VERTEX_BODY: &str = "\
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
return out;";

const ANALYTIC_LINE_HELPERS: &str = "\
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
}";

const ANALYTIC_LINE_FRAGMENT_BODY: &str = "\
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
return out;";

const GLYPHRUN_FRAGMENT_BODY: &str = "\
// Single-channel A8 coverage sampled directly: the atlas texel's red channel
// is exact per-pixel coverage. Modulate the run color by it, premultiplied.
float cov = tex.sample(samp, in.uv).r;
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

const GRADIENT_VERTEX_BODY: &str = "\
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
return out;";

const GRADIENT_HELPERS: &str = "\
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
}";

const GRADIENT_FRAGMENT_BODY: &str = "\
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
return grad * cov;";
