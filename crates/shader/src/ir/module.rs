//! One typed shader IR value describing a built-in primitive's GPU interface —
//! the single source of truth that emits *both* the MSL `InstanceIn`/`VertexIn`
//! struct *and* the [`InstanceSchema`](viso_gpu_abi::InstanceSchema) the pipeline validates
//! against.
//!
//! ## What the IR owns, and what it carries
//!
//! The load-bearing win of the IR is collapsing a duplicated field contract:
//! otherwise each built-in spells its per-instance fields twice — once as a
//! `packed_float*` MSL `struct` and once as a parallel `SchemaAttr` list — kept in
//! lockstep only by name-order test assertions, with byte offsets never
//! cross-checked (an implicit shader instance field-order ABI).
//! Here a [`ShaderIr`]'s [`attributes`](ShaderIr::attributes) field list is the
//! *only* place that contract is written; the MSL struct, the schema, and the
//! registration-time offset expectations all project from it, so they cannot drift.
//!
//! The IR is strongly typed at the **interface** layer — instance/uniform/varying
//! fields, texture/sampler bindings, and buffer indices are typed data. The
//! per-primitive vertex/fragment *algorithm* body is carried as a structured MSL
//! fragment (the exact math the hand-written built-ins already shipped), not an
//! expression AST: building an expression-level shader AST is only needed once
//! users write shader *logic* rather than instantiating built-ins, and that is
//! explicitly out of scope (it would prematurely duplicate the
//! `viso-dsl` frontend). The pipeline shape `source → parsed syntax → typed IR →
//! validation → codegen` is satisfied with a Rust-side structured IR builder as
//! the "parsed syntax → typed IR" front, which is the right front for built-ins.
//!
//! Packing rules and per-primitive math are represented as a typed IR rather
//! than interpreted through a shared script VM.

use viso_gpu_abi::SchemaAttr;

use crate::PrimitiveKind;
use crate::ir::types::{IrType, ScalarType};

/// One typed field of a shader interface `struct` (an instance/vertex attribute,
/// a uniform member, or a varying). The `name` and `ty` are the whole contract;
/// the MSL spelling and the schema/offset projection are derived from `ty`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct IrField {
    /// The field name, shared verbatim by the MSL struct member and the
    /// [`SchemaAttr`] it projects to.
    pub name: &'static str,
    /// The field type — the single input to both the MSL spelling and the
    /// [`AttrFormat`](viso_gpu_abi::AttrFormat) projection.
    pub ty: IrType,
    /// For a uniform-block member, the length of the array it is; `0` for a
    /// single value. Attributes and varyings are never arrays.
    pub count: u32,
}

impl IrField {
    /// Convenience constructor for a `const` field list.
    pub const fn new(name: &'static str, ty: IrType) -> Self {
        IrField { name, ty, count: 0 }
    }

    /// A uniform-block member that is an array of `count` values of `ty`,
    /// indexed in the body as `block.name[i]`.
    pub const fn array(name: &'static str, ty: IrType, count: u32) -> Self {
        IrField { name, ty, count }
    }
}

/// A uniform block beyond the program's `u`: a uniform buffer of its own,
/// read by both stages as the global `name`, a value of struct `struct_name`.
/// Every member is a `float4` or an array of them, so the block lays out the
/// same in every language: 16 bytes a member element, in order.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct IrBlock {
    pub name: &'static str,
    pub struct_name: &'static str,
    pub fields: &'static [IrField],
}

/// A texture a program samples, by its slot's name, and the sampler slot it
/// is sampled through: a texture is only ever sampled through its own
/// sampler (a GLSL ES texture unit carries one).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct IrTexture {
    pub name: &'static str,
    pub sampler: u8,
}

/// The binding slots of a 3D scene program
/// ([`VertexSource::PerVertexInstanced`]), beyond its `u` block at uniform
/// slot 0: the blocks at uniform slots `1..`, the textures at texture slots
/// `0..` and the samplers at sampler slots `0..`, each list in slot order.
/// The fragment stage reads the textures; both stages read the blocks.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SceneBindings {
    pub blocks: &'static [IrBlock],
    pub textures: &'static [IrTexture],
    pub samplers: &'static [&'static str],
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
    /// The vertex shader reads both: a per-vertex buffer of an indexed mesh and
    /// a per-instance buffer of its placements (the scene lane). The vertices
    /// sit at Metal buffer index 0 and the instances at 1. The uniform block is
    /// too large to be inline: it is a uniform buffer at index 2, read by both
    /// stages.
    PerVertexInstanced,
}

impl VertexSource {
    /// The Metal buffer index the attribute buffer (`InstanceIn`/`VertexIn`) binds
    /// to. Per-instance data sits at 1 (uniforms own 0); a per-vertex buffer sits
    /// at 0 (uniforms move to 1).
    pub const fn attr_buffer_index(self) -> u32 {
        match self {
            VertexSource::PerInstance => 1,
            VertexSource::PerVertex | VertexSource::PerVertexInstanced => 0,
        }
    }

    /// The Metal buffer index the `Uniforms` bind to.
    pub const fn uniform_buffer_index(self) -> u32 {
        match self {
            VertexSource::PerInstance => 0,
            VertexSource::PerVertex => 1,
            VertexSource::PerVertexInstanced => 2,
        }
    }

    /// The MSL name of the attribute `struct` this source declares
    /// [`ShaderIr::attributes`] as.
    pub const fn attr_struct_name(self) -> &'static str {
        match self {
            VertexSource::PerInstance => "InstanceIn",
            VertexSource::PerVertex | VertexSource::PerVertexInstanced => "VertexIn",
        }
    }

    /// Whether the uniforms are a bound uniform buffer, read by both stages,
    /// rather than the vertex stage's inline bytes.
    pub const fn uniform_buffer(self) -> bool {
        matches!(self, VertexSource::PerVertexInstanced)
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
    /// truth for the MSL attribute `struct` *and* the [`InstanceSchema`](viso_gpu_abi::InstanceSchema).
    pub attributes: &'static [IrField],
    /// The per-instance fields of a [`VertexSource::PerVertexInstanced`]
    /// source, its `InstanceIn` struct, at the locations after
    /// [`attributes`](Self::attributes); empty for the others.
    pub instance_attributes: &'static [IrField],
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
    /// A scene program's named blocks, textures and samplers; `None` for
    /// every other program, whose textures `texture_count` counts.
    pub bindings: Option<&'static SceneBindings>,
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
    /// `&'static [SchemaAttr]` to satisfy [`InstanceSchema`](viso_gpu_abi::InstanceSchema)'s `'static` borrow —
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
    /// stride. This is the expectation the GPU-side
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

/// The inline uniform block every built-in reads: the target's `viewport`
/// size, and the place its draw goes to — the affine of the transform root the
/// draw belongs to from where its instances were written, so a scroll, slide,
/// zoom or spin moves retained instances without rewriting them: its
/// translation `shift` and the images `axis_x` and `axis_y` of the unit axes.
/// The identity (`shift` zero, unit axes) for a draw outside any moved root.
pub const VIEW_UNIFORMS: &[IrField] = &[
    IrField::new("viewport", IrType::F32X2),
    IrField::new("shift", IrType::F32X2),
    IrField::new("axis_x", IrType::F32X2),
    IrField::new("axis_y", IrType::F32X2),
];

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
    static UNIFORMS: &[IrField] = VIEW_UNIFORMS;
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
        instance_attributes: &[],
        uniforms: UNIFORMS,
        varyings: VARYINGS,
        texture_count: 0,
        bindings: None,
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
        IrField::new("mask_pos", IrType::F32X2),
        IrField::new("mask_size", IrType::F32X2),
        IrField::new("mask_radius", IrType::F32X4),
    ];
    static UNIFORMS: &[IrField] = VIEW_UNIFORMS;
    static VARYINGS: &[Varying] = &[
        Varying::new("position", IrType::F32X4, " [[position]]", ""),
        Varying::new("uv", IrType::F32X2, "", ""),
        Varying::new("tint", IrType::F32X4, "", ""),
        Varying::new("local", IrType::F32X2, "", "point-space sample position"),
        Varying::new("mask_center", IrType::F32X2, "", ""),
        Varying::new("mask_half", IrType::F32X2, "", ""),
        Varying::new(
            "mask_radius",
            IrType::F32X4,
            "",
            "lt, rt, rb, lb; all 0 = unmasked",
        ),
    ];
    ShaderIr {
        kind: PrimitiveKind::Image,
        vertex_source: VertexSource::PerInstance,
        attributes: ATTRS,
        instance_attributes: &[],
        uniforms: UNIFORMS,
        varyings: VARYINGS,
        texture_count: 1,
        bindings: None,
        vertex_body: IMAGE_VERTEX_BODY,
        helpers: IMAGE_HELPERS,
        fragment_body: IMAGE_FRAGMENT_BODY,
    }
}

/// Separable Gaussian blur built-in: one full-target quad sampling a source
/// texture along a single axis. Per-instance data, uniforms at buffer 0, one
/// texture. The blur parameters ride in the instance (`dir`/`sigma`/`radius`)
/// rather than inline uniforms; the vertex stage is the Image quad synthesis and
/// forwards the blur params to the fragment, which runs a 1D Gaussian tap loop
/// over the source uv.
pub fn blur_ir() -> ShaderIr {
    static ATTRS: &[IrField] = &[
        IrField::new("rect_pos", IrType::F32X2),
        IrField::new("rect_size", IrType::F32X2),
        IrField::new("uv_pos", IrType::F32X2),
        IrField::new("uv_size", IrType::F32X2),
        IrField::new("dir", IrType::F32X2),
        IrField::new("sigma", IrType::F32),
        IrField::new("radius", IrType::F32),
    ];
    static UNIFORMS: &[IrField] = VIEW_UNIFORMS;
    static VARYINGS: &[Varying] = &[
        Varying::new("position", IrType::F32X4, " [[position]]", ""),
        Varying::new("uv", IrType::F32X2, "", ""),
        Varying::new("dir", IrType::F32X2, "", "per-tap step in normalized uv"),
        Varying::new("sigma", IrType::F32, "", "Gaussian sigma in source texels"),
        Varying::new("radius", IrType::F32, "", "tap radius (taps each side)"),
        Varying::new(
            "uv_bounds",
            IrType::F32X4,
            "",
            "source sub-rect the taps clamp to: (min_uv, max_uv)",
        ),
    ];
    ShaderIr {
        kind: PrimitiveKind::Blur,
        vertex_source: VertexSource::PerInstance,
        attributes: ATTRS,
        instance_attributes: &[],
        uniforms: UNIFORMS,
        varyings: VARYINGS,
        texture_count: 1,
        bindings: None,
        vertex_body: BLUR_VERTEX_BODY,
        helpers: "",
        fragment_body: BLUR_FRAGMENT_BODY,
    }
}

/// Fused color-effect built-in: one quad sampling a source texture and mapping
/// each texel through an affine color matrix plus an optional gamma. Per-instance
/// data, uniforms at buffer 0, one texture.
///
/// The whole point is that a *run* of color effects (brightness, contrast,
/// saturation, hue rotation, …) multiplies down to one 4×5 matrix, so this single
/// pass realizes as many effects as the math can merge; the fragment stage never
/// sees how many effects it stands for. `row0..row3` are the output rows over
/// `(r, g, b, a)`, `offset` is the matrix's fifth (constant) column, and `gamma`
/// is the one stage matrices cannot express.
pub fn color_transform_ir() -> ShaderIr {
    static ATTRS: &[IrField] = &[
        IrField::new("rect_pos", IrType::F32X2),
        IrField::new("rect_size", IrType::F32X2),
        IrField::new("uv_pos", IrType::F32X2),
        IrField::new("uv_size", IrType::F32X2),
        IrField::new("row0", IrType::F32X4),
        IrField::new("row1", IrType::F32X4),
        IrField::new("row2", IrType::F32X4),
        IrField::new("row3", IrType::F32X4),
        IrField::new("offset", IrType::F32X4),
        IrField::new("gamma", IrType::F32),
    ];
    static UNIFORMS: &[IrField] = VIEW_UNIFORMS;
    static VARYINGS: &[Varying] = &[
        Varying::new("position", IrType::F32X4, " [[position]]", ""),
        Varying::new("uv", IrType::F32X2, "", ""),
        Varying::new("row0", IrType::F32X4, "", "red row over (r, g, b, a)"),
        Varying::new("row1", IrType::F32X4, "", "green row"),
        Varying::new("row2", IrType::F32X4, "", "blue row"),
        Varying::new("row3", IrType::F32X4, "", "alpha row"),
        Varying::new("offset", IrType::F32X4, "", "constant column per channel"),
        Varying::new("gamma", IrType::F32, "", "post-matrix RGB exponent"),
    ];
    ShaderIr {
        kind: PrimitiveKind::ColorTransform,
        vertex_source: VertexSource::PerInstance,
        attributes: ATTRS,
        instance_attributes: &[],
        uniforms: UNIFORMS,
        varyings: VARYINGS,
        texture_count: 1,
        bindings: None,
        vertex_body: COLOR_TRANSFORM_VERTEX_BODY,
        helpers: "",
        fragment_body: COLOR_TRANSFORM_FRAGMENT_BODY,
    }
}

/// Output-encode built-in: the last pass of a frame drawn into a working
/// target, for a surface whose format cannot encode (a unorm window, an
/// `Rgb565Unorm` panel). One quad samples the working target, whose texels are
/// premultiplied linear light, and writes them through the sRGB transfer
/// function, the encode the surface itself would otherwise have done; an
/// ordered dither of `dither` (one output step, `0` = none) keeps a narrow
/// panel's ramps from banding.
pub fn output_encode_ir() -> ShaderIr {
    static ATTRS: &[IrField] = &[
        IrField::new("rect_pos", IrType::F32X2),
        IrField::new("rect_size", IrType::F32X2),
        IrField::new("uv_pos", IrType::F32X2),
        IrField::new("uv_size", IrType::F32X2),
        IrField::new("dither", IrType::F32),
    ];
    static UNIFORMS: &[IrField] = VIEW_UNIFORMS;
    static VARYINGS: &[Varying] = &[
        Varying::new("position", IrType::F32X4, " [[position]]", ""),
        Varying::new("uv", IrType::F32X2, "", ""),
        Varying::new("local", IrType::F32X2, "", "pixel-space sample position"),
        Varying::new("dither", IrType::F32, "", "one output step, 0 = none"),
    ];
    ShaderIr {
        kind: PrimitiveKind::OutputEncode,
        vertex_source: VertexSource::PerInstance,
        attributes: ATTRS,
        instance_attributes: &[],
        uniforms: UNIFORMS,
        varyings: VARYINGS,
        texture_count: 1,
        bindings: None,
        vertex_body: OUTPUT_ENCODE_VERTEX_BODY,
        helpers: OUTPUT_ENCODE_HELPERS,
        fragment_body: OUTPUT_ENCODE_FRAGMENT_BODY,
    }
}

/// Advanced-blend built-in: the isolated composite of a layer that asked for a
/// blend mode the fixed-function stage cannot express. Per-instance data,
/// uniforms at buffer 0, **two** textures — the isolated layer (source) at slot 0
/// and a bounded snapshot of what is behind it (destination) at slot 1.
///
/// The whole blend is evaluated in the fragment stage and written with
/// `BlendMode::Replace`, so the destination is read exactly once and mixed in
/// exactly once. `mode` is the blend discriminant (the render crate's `Blend`
/// ABI): `0..=12` are the Porter-Duff separable-coverage modes, `13..=23` the
/// W3C separable artistic ones, `24..=27` the non-separable HSL ones.
pub fn advanced_blend_ir() -> ShaderIr {
    static ATTRS: &[IrField] = &[
        IrField::new("rect_pos", IrType::F32X2),
        IrField::new("rect_size", IrType::F32X2),
        IrField::new("uv_pos", IrType::F32X2),
        IrField::new("uv_size", IrType::F32X2),
        IrField::new("dst_uv_pos", IrType::F32X2),
        IrField::new("dst_uv_size", IrType::F32X2),
        IrField::new("mode", IrType::U32),
        IrField::new("opacity", IrType::F32),
    ];
    static UNIFORMS: &[IrField] = VIEW_UNIFORMS;
    static VARYINGS: &[Varying] = &[
        Varying::new("position", IrType::F32X4, " [[position]]", ""),
        Varying::new("uv", IrType::F32X2, "", "source (isolated layer) uv"),
        Varying::new("dst_uv", IrType::F32X2, "", "destination (snapshot) uv"),
        Varying::new(
            "mode",
            IrType::U32,
            " [[flat]]",
            "0..12 Porter-Duff 13..23 separable 24..27 HSL",
        ),
        Varying::new("opacity", IrType::F32, "", "layer opacity, folded into src"),
    ];
    ShaderIr {
        kind: PrimitiveKind::AdvancedBlend,
        vertex_source: VertexSource::PerInstance,
        attributes: ATTRS,
        instance_attributes: &[],
        uniforms: UNIFORMS,
        varyings: VARYINGS,
        texture_count: 2,
        bindings: None,
        vertex_body: ADVANCED_BLEND_VERTEX_BODY,
        helpers: ADVANCED_BLEND_HELPERS,
        fragment_body: ADVANCED_BLEND_FRAGMENT_BODY,
    }
}

/// Material-composite built-in: the one draw a frosted surface costs.
///
/// A frosted panel is `backdrop → blur → saturation/tint → optional noise →
/// material mask → border/highlight`. The first two stages are passes that
/// already exist (the backdrop capture and its blur ladder), and the last is an
/// ordinary analytic rounded-rect drawn over this one. Everything between fuses
/// into this single fragment, so a panel adds no pass, no target and no capture
/// beyond what a plain blurred-backdrop layer already needed:
///
/// - the tint is the same affine 4×5 color matrix (`row0..row3` + `offset`) plus
///   `gamma` the fused color built-in uses, so saturation/tint/brightness cost
///   nothing extra here;
/// - `noise` is a grain amplitude over a hash of the *integer device pixel*, so a
///   static screen renders byte-identically every frame (no time input);
/// - `radius` is the panel's per-corner mask, evaluated as a signed distance like
///   every other analytic shape, so the glass has real antialiased corners
///   instead of a rectangular cut;
/// - `opacity` scales the whole result last.
///
/// Per-instance data, uniforms at buffer 0, one texture (the blurred backdrop).
pub fn material_ir() -> ShaderIr {
    static ATTRS: &[IrField] = &[
        IrField::new("rect_pos", IrType::F32X2),
        IrField::new("rect_size", IrType::F32X2),
        IrField::new("uv_pos", IrType::F32X2),
        IrField::new("uv_size", IrType::F32X2),
        IrField::new("row0", IrType::F32X4),
        IrField::new("row1", IrType::F32X4),
        IrField::new("row2", IrType::F32X4),
        IrField::new("row3", IrType::F32X4),
        IrField::new("offset", IrType::F32X4),
        IrField::new("radius", IrType::F32X4),
        IrField::new("gamma", IrType::F32),
        IrField::new("noise", IrType::F32),
        IrField::new("opacity", IrType::F32),
    ];
    static UNIFORMS: &[IrField] = VIEW_UNIFORMS;
    static VARYINGS: &[Varying] = &[
        Varying::new("position", IrType::F32X4, " [[position]]", ""),
        Varying::new("uv", IrType::F32X2, "", "backdrop uv"),
        Varying::new("uv_bounds", IrType::F32X4, "", "backdrop sub-rect (lo, hi)"),
        Varying::new("local", IrType::F32X2, "", "pixel-space sample position"),
        Varying::new("half_size", IrType::F32X2, "", ""),
        Varying::new("center", IrType::F32X2, "", ""),
        Varying::new("radius", IrType::F32X4, "", "mask radii lt, rt, rb, lb"),
        Varying::new("row0", IrType::F32X4, "", "red row over (r, g, b, a)"),
        Varying::new("row1", IrType::F32X4, "", "green row"),
        Varying::new("row2", IrType::F32X4, "", "blue row"),
        Varying::new("row3", IrType::F32X4, "", "alpha row"),
        Varying::new("offset", IrType::F32X4, "", "constant column per channel"),
        Varying::new("gamma", IrType::F32, "", "post-matrix RGB exponent"),
        Varying::new("noise", IrType::F32, "", "grain amplitude, 0 = none"),
        Varying::new("opacity", IrType::F32, "", ""),
    ];
    ShaderIr {
        kind: PrimitiveKind::Material,
        vertex_source: VertexSource::PerInstance,
        attributes: ATTRS,
        instance_attributes: &[],
        uniforms: UNIFORMS,
        varyings: VARYINGS,
        texture_count: 1,
        bindings: None,
        vertex_body: MATERIAL_VERTEX_BODY,
        helpers: MATERIAL_HELPERS,
        fragment_body: MATERIAL_FRAGMENT_BODY,
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
    static UNIFORMS: &[IrField] = VIEW_UNIFORMS;
    static VARYINGS: &[Varying] = &[
        Varying::new("position", IrType::F32X4, " [[position]]", ""),
        Varying::new("uv", IrType::F32X2, "", ""),
        Varying::new("color", IrType::F32X4, "", ""),
    ];
    ShaderIr {
        kind: PrimitiveKind::GlyphRun,
        vertex_source: VertexSource::PerInstance,
        attributes: ATTRS,
        instance_attributes: &[],
        uniforms: UNIFORMS,
        varyings: VARYINGS,
        texture_count: 1,
        bindings: None,
        vertex_body: GLYPHRUN_VERTEX_BODY,
        helpers: INK_HELPERS,
        fragment_body: GLYPHRUN_FRAGMENT_BODY,
    }
}

/// MTSDF built-in: the scalable text lane. Same instance ABI as the glyph-run
/// built-in — the same atlas quad, the same run color — but the atlas holds a
/// multi-channel signed distance field instead of coverage, so one field stays
/// sharp across its whole quality window instead of one fixed size.
///
/// Distinct from the coverage lane rather than a variant of it: the fragment
/// decode is different work, and a field glyph and a coverage glyph live in
/// different atlases. Viso 1.0 has no third, single-channel lane.
pub fn mtsdf_ir() -> ShaderIr {
    static ATTRS: &[IrField] = &[
        IrField::new("rect_pos", IrType::F32X2),
        IrField::new("rect_size", IrType::F32X2),
        IrField::new("uv_pos", IrType::F32X2),
        IrField::new("uv_size", IrType::F32X2),
        IrField::new("color", IrType::F32X4),
    ];
    static UNIFORMS: &[IrField] = VIEW_UNIFORMS;
    static VARYINGS: &[Varying] = &[
        Varying::new("position", IrType::F32X4, " [[position]]", ""),
        Varying::new("uv", IrType::F32X2, "", ""),
        Varying::new("color", IrType::F32X4, "", ""),
    ];
    ShaderIr {
        kind: PrimitiveKind::Mtsdf,
        vertex_source: VertexSource::PerInstance,
        attributes: ATTRS,
        instance_attributes: &[],
        uniforms: UNIFORMS,
        varyings: VARYINGS,
        texture_count: 1,
        bindings: None,
        vertex_body: MTSDF_VERTEX_BODY,
        helpers: INK_HELPERS,
        fragment_body: MTSDF_FRAGMENT_BODY,
    }
}

/// General mesh built-in (shared by Path and Mesh): a real per-vertex buffer at
/// index 0, uniforms at index 1, no instance buffer, no texture.
pub fn mesh_ir() -> ShaderIr {
    static ATTRS: &[IrField] = &[
        IrField::new("pos", IrType::F32X2),
        IrField::new("color", IrType::F32X4),
        IrField::new("edge", IrType::F32),
        IrField::new("extrude", IrType::F32X2),
    ];
    static UNIFORMS: &[IrField] = VIEW_UNIFORMS;
    static VARYINGS: &[Varying] = &[
        Varying::new("position", IrType::F32X4, " [[position]]", ""),
        Varying::new("color", IrType::F32X4, "", ""),
        Varying::new("edge", IrType::F32, "", ""),
    ];
    ShaderIr {
        kind: PrimitiveKind::Mesh,
        vertex_source: VertexSource::PerVertex,
        attributes: ATTRS,
        instance_attributes: &[],
        uniforms: UNIFORMS,
        varyings: VARYINGS,
        texture_count: 0,
        bindings: None,
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
    static UNIFORMS: &[IrField] = VIEW_UNIFORMS;
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
        instance_attributes: &[],
        uniforms: UNIFORMS,
        varyings: VARYINGS,
        texture_count: 0,
        bindings: None,
        vertex_body: ANALYTIC_RRECT_VERTEX_BODY,
        helpers: ANALYTIC_RRECT_HELPERS,
        fragment_body: ANALYTIC_RRECT_FRAGMENT_BODY,
    }
}

/// Analytic shadow fast lane: a soft drop shadow for
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
    static UNIFORMS: &[IrField] = VIEW_UNIFORMS;
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
        instance_attributes: &[],
        uniforms: UNIFORMS,
        varyings: VARYINGS,
        texture_count: 0,
        bindings: None,
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
    static UNIFORMS: &[IrField] = VIEW_UNIFORMS;
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
        instance_attributes: &[],
        uniforms: UNIFORMS,
        varyings: VARYINGS,
        texture_count: 0,
        bindings: None,
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
    static UNIFORMS: &[IrField] = VIEW_UNIFORMS;
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
        instance_attributes: &[],
        uniforms: UNIFORMS,
        varyings: VARYINGS,
        texture_count: 0,
        bindings: None,
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
    static UNIFORMS: &[IrField] = VIEW_UNIFORMS;
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
        instance_attributes: &[],
        uniforms: UNIFORMS,
        varyings: VARYINGS,
        texture_count: 0,
        bindings: None,
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
    static UNIFORMS: &[IrField] = VIEW_UNIFORMS;
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
        instance_attributes: &[],
        uniforms: UNIFORMS,
        varyings: VARYINGS,
        texture_count: 1,
        bindings: None,
        vertex_body: GRADIENT_VERTEX_BODY,
        helpers: GRADIENT_HELPERS,
        fragment_body: GRADIENT_FRAGMENT_BODY,
    }
}

/// The frame's uniform block of every scene program, `u`:
/// `viso_gpu_abi::scene::FrameUniforms`, its column-major view-projection as
/// four columns.
static SCENE_FRAME: &[IrField] = &[
    IrField::new("view_proj0", IrType::F32X4),
    IrField::new("view_proj1", IrType::F32X4),
    IrField::new("view_proj2", IrType::F32X4),
    IrField::new("view_proj3", IrType::F32X4),
    IrField::new("camera", IrType::F32X4),
    IrField::new("light_dir", IrType::F32X4),
    IrField::new("light_color", IrType::F32X4),
    IrField::new("fog", IrType::F32X4),
    IrField::new("ambient", IrType::F32X4),
    IrField::array("sh", IrType::F32X4, 9),
    IrField::array("shadow", IrType::F32X4, 12),
    IrField::new("shadow_params", IrType::F32X4),
    IrField::new("occlusion", IrType::F32X4),
];

/// A scene mesh's vertex: `viso_gpu_abi::scene::SceneVertex`.
static SCENE_ATTRS: &[IrField] = &[
    IrField::new("position", IrType::F32X3),
    IrField::new("normal", IrType::F32X3),
    IrField::new("uv", IrType::F32X2),
    IrField::new("tangent", IrType::F32X4),
];

/// A skinned scene mesh's vertex: `viso_gpu_abi::scene::SkinnedVertex`.
static SCENE_SKINNED_ATTRS: &[IrField] = &[
    IrField::new("position", IrType::F32X3),
    IrField::new("normal", IrType::F32X3),
    IrField::new("uv", IrType::F32X2),
    IrField::new("tangent", IrType::F32X4),
    IrField::new(
        "joints",
        IrType::Vector {
            scalar: ScalarType::U32,
            lanes: 2,
        },
    ),
    IrField::new(
        "weights",
        IrType::Vector {
            scalar: ScalarType::U32,
            lanes: 2,
        },
    ),
];

/// The attributes of a `viso_gpu_abi::scene::VertexExtra`, after a
/// vertex's own: its second texture coordinate set and its color, four
/// 16-bit fractions two to a word.
macro_rules! scene_extra_attrs {
    () => {
        [
            IrField::new("uv1", IrType::F32X2),
            IrField::new(
                "color",
                IrType::Vector {
                    scalar: ScalarType::U32,
                    lanes: 2,
                },
            ),
        ]
    };
}

/// A scene mesh's vertex with its extra.
static SCENE_EXTRA_ATTRS: &[IrField] = &{
    let [uv1, color] = scene_extra_attrs!();
    [
        SCENE_ATTRS[0],
        SCENE_ATTRS[1],
        SCENE_ATTRS[2],
        SCENE_ATTRS[3],
        uv1,
        color,
    ]
};

/// A skinned scene mesh's vertex with its extra.
static SCENE_EXTRA_SKINNED_ATTRS: &[IrField] = &{
    let [uv1, color] = scene_extra_attrs!();
    let a = SCENE_SKINNED_ATTRS;
    [a[0], a[1], a[2], a[3], a[4], a[5], uv1, color]
};

/// A skinned draw's joint palette, `skin`: three rows of each joint's
/// affine, `viso_gpu_abi::scene::MAX_JOINTS` joints.
const SCENE_SKIN: IrBlock = IrBlock {
    name: "skin",
    struct_name: "SkinBlock",
    fields: &[IrField::array("joints", IrType::F32X4, 768)],
};

/// A scene draw's instance: `viso_gpu_abi::scene::SceneInstance`.
static SCENE_INSTANCE: &[IrField] = &[
    IrField::new("row0", IrType::F32X4),
    IrField::new("row1", IrType::F32X4),
    IrField::new("row2", IrType::F32X4),
    IrField::new("color", IrType::F32X4),
];

/// A material's uniform block, `material`:
/// `viso_gpu_abi::scene::MaterialUniforms`.
const SCENE_MATERIAL: IrBlock = IrBlock {
    name: "material",
    struct_name: "MaterialBlock",
    fields: &[
        IrField::new("base_color", IrType::F32X4),
        IrField::new("emissive", IrType::F32X4),
        IrField::new("params", IrType::F32X4),
        IrField::new("mode", IrType::F32X4),
    ],
};

/// The material's maps, sampled through sampler slot 0.
const SCENE_MATERIAL_TEXTURES: [IrTexture; 5] = [
    IrTexture {
        name: "base_tex",
        sampler: 0,
    },
    IrTexture {
        name: "mr_tex",
        sampler: 0,
    },
    IrTexture {
        name: "normal_tex",
        sampler: 0,
    },
    IrTexture {
        name: "occlusion_tex",
        sampler: 0,
    },
    IrTexture {
        name: "emissive_tex",
        sampler: 0,
    },
];

/// The mesh program's varyings.
static SCENE_MESH_VARYINGS: &[Varying] = &[
    Varying::new("position", IrType::F32X4, " [[position]]", ""),
    Varying::new("world", IrType::F32X3, "", ""),
    Varying::new("normal", IrType::F32X3, "", ""),
    Varying::new("tangent", IrType::F32X4, "", ""),
    Varying::new("uv", IrType::F32X2, "", ""),
    Varying::new("color", IrType::F32X4, " [[flat]]", ""),
];

/// The mesh program's varyings over vertices with extras: the second
/// texture coordinate set and the vertex color too.
static SCENE_EXTRA_MESH_VARYINGS: &[Varying] = &[
    Varying::new("position", IrType::F32X4, " [[position]]", ""),
    Varying::new("world", IrType::F32X3, "", ""),
    Varying::new("normal", IrType::F32X3, "", ""),
    Varying::new("tangent", IrType::F32X4, "", ""),
    Varying::new("uv", IrType::F32X2, "", ""),
    Varying::new("color", IrType::F32X4, " [[flat]]", ""),
    Varying::new("uv1", IrType::F32X2, "", ""),
    Varying::new("vertex_color", IrType::F32X4, "", ""),
];

/// The mesh program's resources past the frame's: the material's block, its
/// maps, the environment and the shadow atlas.
static SCENE_MESH_BINDINGS: SceneBindings = SceneBindings {
    blocks: &[SCENE_MATERIAL],
    textures: &[
        SCENE_MATERIAL_TEXTURES[0],
        SCENE_MATERIAL_TEXTURES[1],
        SCENE_MATERIAL_TEXTURES[2],
        SCENE_MATERIAL_TEXTURES[3],
        SCENE_MATERIAL_TEXTURES[4],
        IrTexture {
            name: "env_tex",
            sampler: 1,
        },
        IrTexture {
            name: "shadow_tex",
            sampler: 2,
        },
        IrTexture {
            name: "ao_tex",
            sampler: 1,
        },
    ],
    samplers: &["material_samp", "linear_samp", "point_samp"],
};

/// [`SCENE_MESH_BINDINGS`] with the joint palette.
static SCENE_SKINNED_MESH_BINDINGS: SceneBindings = SceneBindings {
    blocks: &[SCENE_MATERIAL, SCENE_SKIN],
    textures: SCENE_MESH_BINDINGS.textures,
    samplers: SCENE_MESH_BINDINGS.samplers,
};

/// The depth program's varyings.
static SCENE_DEPTH_VARYINGS: &[Varying] = &[
    Varying::new("position", IrType::F32X4, " [[position]]", ""),
    Varying::new("uv", IrType::F32X2, "", ""),
    Varying::new("light_depth", IrType::F32, "", ""),
    Varying::new("tint_alpha", IrType::F32, " [[flat]]", ""),
];

/// The depth program's varyings over vertices with extras: the second
/// texture coordinate set and the vertex color's alpha too.
static SCENE_EXTRA_DEPTH_VARYINGS: &[Varying] = &[
    Varying::new("position", IrType::F32X4, " [[position]]", ""),
    Varying::new("uv", IrType::F32X2, "", ""),
    Varying::new("light_depth", IrType::F32, "", ""),
    Varying::new("tint_alpha", IrType::F32, " [[flat]]", ""),
    Varying::new("uv1", IrType::F32X2, "", ""),
    Varying::new("vertex_alpha", IrType::F32, "", ""),
];

/// The depth program's resources past the frame's: the material's block
/// and its base color, for a mask.
static SCENE_DEPTH_BINDINGS: SceneBindings = SceneBindings {
    blocks: &[SCENE_MATERIAL],
    textures: &[SCENE_MATERIAL_TEXTURES[0]],
    samplers: &["material_samp"],
};

/// [`SCENE_DEPTH_BINDINGS`] with the joint palette.
static SCENE_SKINNED_DEPTH_BINDINGS: SceneBindings = SceneBindings {
    blocks: &[SCENE_MATERIAL, SCENE_SKIN],
    textures: SCENE_DEPTH_BINDINGS.textures,
    samplers: SCENE_DEPTH_BINDINGS.samplers,
};

/// The sky's uniform block, `sky`: `viso_gpu_abi::scene::SkyUniforms`,
/// its column-major inverse view-projection as four columns.
const SCENE_SKY: IrBlock = IrBlock {
    name: "sky",
    struct_name: "SkyBlock",
    fields: &[
        IrField::new("inv_view_proj0", IrType::F32X4),
        IrField::new("inv_view_proj1", IrType::F32X4),
        IrField::new("inv_view_proj2", IrType::F32X4),
        IrField::new("inv_view_proj3", IrType::F32X4),
        IrField::new("perez_y", IrType::F32X4),
        IrField::new("perez_x", IrType::F32X4),
        IrField::new("perez_yc", IrType::F32X4),
        IrField::new("perez_e", IrType::F32X4),
        IrField::new("zenith", IrType::F32X4),
        IrField::new("sun_model", IrType::F32X4),
        IrField::new("sun", IrType::F32X4),
        IrField::new("sun_radiance", IrType::F32X4),
        IrField::new("blend", IrType::F32X4),
    ],
};

/// A screen program's varyings: its clip-space position over the target.
static SCENE_SCREEN_VARYINGS: &[Varying] = &[
    Varying::new("position", IrType::F32X4, " [[position]]", ""),
    Varying::new("ndc", IrType::F32X2, "", ""),
];

/// The sky program's resources past the frame's: its block.
static SCENE_SKY_BINDINGS: SceneBindings = SceneBindings {
    blocks: &[SCENE_SKY],
    textures: &[],
    samplers: &[],
};

/// The ambient occlusion passes' uniform block, `ao`:
/// `viso_gpu_abi::scene::AoUniforms`, its column-major inverse
/// view-projection as four columns.
const SCENE_AO: IrBlock = IrBlock {
    name: "ao",
    struct_name: "AoBlock",
    fields: &[
        IrField::new("inv_view_proj0", IrType::F32X4),
        IrField::new("inv_view_proj1", IrType::F32X4),
        IrField::new("inv_view_proj2", IrType::F32X4),
        IrField::new("inv_view_proj3", IrType::F32X4),
        IrField::new("params", IrType::F32X4),
        IrField::new("texel", IrType::F32X4),
        IrField::new("step", IrType::F32X4),
    ],
};

/// A texture slot an occlusion pass binds but does not read.
const fn unread(name: &'static str) -> IrTexture {
    IrTexture { name, sampler: 0 }
}

/// The occlusion passes' resources past the frame's: their block, and
/// what they read in texture slot 6 through the nearest sampler.
static SCENE_AO_BINDINGS: SceneBindings = SceneBindings {
    blocks: &[SCENE_AO],
    textures: &[
        unread("unread0"),
        unread("unread1"),
        unread("unread2"),
        unread("unread3"),
        unread("unread4"),
        unread("unread5"),
        IrTexture {
            name: "depth_tex",
            sampler: 2,
        },
    ],
    samplers: &["unread_samp0", "unread_samp1", "point_samp"],
};

/// A scene program over vertices `attributes`.
fn scene_program(
    attributes: &'static [IrField],
    varyings: &'static [Varying],
    bindings: &'static SceneBindings,
    vertex_body: &'static str,
    helpers: &'static str,
    fragment_body: &'static str,
) -> ShaderIr {
    ShaderIr {
        kind: PrimitiveKind::Scene,
        vertex_source: VertexSource::PerVertexInstanced,
        attributes,
        instance_attributes: SCENE_INSTANCE,
        uniforms: SCENE_FRAME,
        varyings,
        texture_count: 0,
        bindings: Some(bindings),
        vertex_body,
        helpers,
        fragment_body,
    }
}

/// The 3D scene's mesh program (`viso_gpu_abi::scene::SceneProgram::Mesh`):
/// an indexed mesh drawn once per instance in its material. The vertex and
/// instance layouts are `SceneVertex` and `SceneInstance`; the fragment
/// computes `viso_gpu_abi::scene::shade_mesh`, the expression the CPU raster
/// runs.
pub fn scene_ir() -> ShaderIr {
    scene_program(
        SCENE_ATTRS,
        SCENE_MESH_VARYINGS,
        &SCENE_MESH_BINDINGS,
        SCENE_VERTEX_BODY,
        SCENE_HELPERS,
        SCENE_FRAGMENT_BODY,
    )
}

/// The 3D scene's shadow caster program
/// (`viso_gpu_abi::scene::SceneProgram::Depth`): a mesh's depth from the
/// light, packed into an RGBA8 texel of the shadow atlas, as
/// `viso_gpu_abi::scene::shade_depth` computes it. The layouts and the
/// bind group are the mesh program's.
pub fn scene_depth_ir() -> ShaderIr {
    scene_program(
        SCENE_ATTRS,
        SCENE_DEPTH_VARYINGS,
        &SCENE_DEPTH_BINDINGS,
        SCENE_DEPTH_VERTEX_BODY,
        "",
        SCENE_DEPTH_FRAGMENT_BODY,
    )
}

/// The mesh program over a skinned mesh
/// (`viso_gpu_abi::scene::SceneProgram::SkinnedMesh`): each
/// `SkinnedVertex` moved by its joints' palette in uniform slot 2 first, as
/// `viso_gpu_abi::scene::skin_vertex` moves it.
pub fn scene_skinned_ir() -> ShaderIr {
    scene_program(
        SCENE_SKINNED_ATTRS,
        SCENE_MESH_VARYINGS,
        &SCENE_SKINNED_MESH_BINDINGS,
        SCENE_SKINNED_VERTEX_BODY,
        SCENE_HELPERS,
        SCENE_SKINNED_FRAGMENT_BODY,
    )
}

/// The depth program over a skinned mesh
/// (`viso_gpu_abi::scene::SceneProgram::SkinnedDepth`).
pub fn scene_skinned_depth_ir() -> ShaderIr {
    scene_program(
        SCENE_SKINNED_ATTRS,
        SCENE_DEPTH_VARYINGS,
        &SCENE_SKINNED_DEPTH_BINDINGS,
        SCENE_SKINNED_DEPTH_VERTEX_BODY,
        "",
        SCENE_DEPTH_FRAGMENT_BODY,
    )
}

/// The sky behind a scene's meshes
/// (`viso_gpu_abi::scene::SceneProgram::Sky`): a triangle over the target
/// at the far plane, each fragment the Preetham sky along its view ray, as
/// `viso_gpu_abi::scene::shade_sky` computes it.
pub fn scene_sky_ir() -> ShaderIr {
    scene_program(
        SCENE_ATTRS,
        SCENE_SCREEN_VARYINGS,
        &SCENE_SKY_BINDINGS,
        SCENE_SCREEN_VERTEX_BODY,
        SCENE_HELPERS,
        SCENE_SKY_FRAGMENT_BODY,
    )
}

/// The ambient occlusion of a view
/// (`viso_gpu_abi::scene::SceneProgram::Ao`): a triangle over the
/// occlusion target, each fragment the hemisphere taps about the surface
/// the view's depth holds there, as `viso_gpu_abi::scene::shade_ao`
/// computes them.
pub fn scene_ao_ir() -> ShaderIr {
    scene_program(
        SCENE_ATTRS,
        SCENE_SCREEN_VARYINGS,
        &SCENE_AO_BINDINGS,
        SCENE_SCREEN_VERTEX_BODY,
        SCENE_AO_HELPERS,
        SCENE_AO_FRAGMENT_BODY,
    )
}

/// One axis of the depth-aware blur of the ambient occlusion
/// (`viso_gpu_abi::scene::SceneProgram::AoBlur`), as
/// `viso_gpu_abi::scene::shade_ao_blur` computes it.
pub fn scene_ao_blur_ir() -> ShaderIr {
    scene_program(
        SCENE_ATTRS,
        SCENE_SCREEN_VARYINGS,
        &SCENE_AO_BINDINGS,
        SCENE_SCREEN_VERTEX_BODY,
        "",
        SCENE_AO_BLUR_FRAGMENT_BODY,
    )
}

/// The mesh program over vertices that each carry a
/// `viso_gpu_abi::scene::VertexExtra`
/// (`viso_gpu_abi::scene::SceneProgram::ExtraMesh`): a second texture
/// coordinate set the material's maps may read, and a color multiplying
/// the base color.
pub fn scene_extra_ir() -> ShaderIr {
    scene_program(
        SCENE_EXTRA_ATTRS,
        SCENE_EXTRA_MESH_VARYINGS,
        &SCENE_MESH_BINDINGS,
        SCENE_EXTRA_VERTEX_BODY,
        SCENE_HELPERS,
        SCENE_EXTRA_FRAGMENT_BODY,
    )
}

/// The depth program over vertices with extras
/// (`viso_gpu_abi::scene::SceneProgram::ExtraDepth`).
pub fn scene_extra_depth_ir() -> ShaderIr {
    scene_program(
        SCENE_EXTRA_ATTRS,
        SCENE_EXTRA_DEPTH_VARYINGS,
        &SCENE_DEPTH_BINDINGS,
        SCENE_EXTRA_DEPTH_VERTEX_BODY,
        "",
        SCENE_EXTRA_DEPTH_FRAGMENT_BODY,
    )
}

/// The skinned mesh program over vertices with extras
/// (`viso_gpu_abi::scene::SceneProgram::ExtraSkinnedMesh`).
pub fn scene_extra_skinned_ir() -> ShaderIr {
    scene_program(
        SCENE_EXTRA_SKINNED_ATTRS,
        SCENE_EXTRA_MESH_VARYINGS,
        &SCENE_SKINNED_MESH_BINDINGS,
        SCENE_EXTRA_SKINNED_VERTEX_BODY,
        SCENE_HELPERS,
        SCENE_EXTRA_SKINNED_FRAGMENT_BODY,
    )
}

/// The skinned depth program over vertices with extras
/// (`viso_gpu_abi::scene::SceneProgram::ExtraSkinnedDepth`).
pub fn scene_extra_skinned_depth_ir() -> ShaderIr {
    scene_program(
        SCENE_EXTRA_SKINNED_ATTRS,
        SCENE_EXTRA_DEPTH_VARYINGS,
        &SCENE_SKINNED_DEPTH_BINDINGS,
        SCENE_EXTRA_SKINNED_DEPTH_VERTEX_BODY,
        "",
        SCENE_EXTRA_DEPTH_FRAGMENT_BODY,
    )
}

/// The scene lane's programs past its mesh program (the [`PrimitiveKind::Scene`]
/// built-in), in `viso_gpu_abi::scene::SceneProgram` order from code 1.
pub fn scene_program_irs() -> [ShaderIr; 10] {
    [
        scene_depth_ir(),
        scene_skinned_ir(),
        scene_skinned_depth_ir(),
        scene_sky_ir(),
        scene_ao_ir(),
        scene_ao_blur_ir(),
        scene_extra_ir(),
        scene_extra_depth_ir(),
        scene_extra_skinned_ir(),
        scene_extra_skinned_depth_ir(),
    ]
}

/// Every program's IR: the built-ins, then the scene lane's other programs —
/// the set the portable codegens and their oracles cover.
pub fn every_ir() -> Vec<ShaderIr> {
    builtin_irs()
        .into_iter()
        .chain(scene_program_irs())
        .collect()
}

/// Every built-in's IR, in [`PrimitiveKind`] declaration order.
pub fn builtin_irs() -> [ShaderIr; 17] {
    [
        quad_ir(),
        image_ir(),
        blur_ir(),
        color_transform_ir(),
        advanced_blend_ir(),
        material_ir(),
        glyphrun_ir(),
        mtsdf_ir(),
        mesh_ir(),
        analytic_rrect_ir(),
        analytic_shadow_ir(),
        analytic_ellipse_ir(),
        analytic_capsule_ir(),
        analytic_line_ir(),
        gradient_ir(),
        scene_ir(),
        output_encode_ir(),
    ]
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
float2 at = float2(u.axis_x) * pixel.x + float2(u.axis_y) * pixel.y + float2(u.shift);
float2 ndc = float2(at.x / vp.x * 2.0 - 1.0,
                    1.0 - at.y / vp.y * 2.0);

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
float2 at = float2(u.axis_x) * pixel.x + float2(u.axis_y) * pixel.y + float2(u.shift);
float2 ndc = float2(at.x / vp.x * 2.0 - 1.0,
                    1.0 - at.y / vp.y * 2.0);

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
float2 at = float2(u.axis_x) * pixel.x + float2(u.axis_y) * pixel.y + float2(u.shift);
float2 ndc = float2(at.x / vp.x * 2.0 - 1.0,
                    1.0 - at.y / vp.y * 2.0);

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
float2 at = float2(u.axis_x) * pixel.x + float2(u.axis_y) * pixel.y + float2(u.shift);
float2 ndc = float2(at.x / vp.x * 2.0 - 1.0,
                    1.0 - at.y / vp.y * 2.0);

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
float2 at = float2(u.axis_x) * pixel.x + float2(u.axis_y) * pixel.y + float2(u.shift);
float2 ndc = float2(at.x / vp.x * 2.0 - 1.0,
                    1.0 - at.y / vp.y * 2.0);

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
float2 at = float2(u.axis_x) * pixel.x + float2(u.axis_y) * pixel.y + float2(u.shift);
float2 ndc = float2(at.x / vp.x * 2.0 - 1.0,
                    1.0 - at.y / vp.y * 2.0);

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
float2 at = float2(u.axis_x) * pixel.x + float2(u.axis_y) * pixel.y + float2(u.shift);
float2 ndc = float2(at.x / vp.x * 2.0 - 1.0,
                    1.0 - at.y / vp.y * 2.0);

VOut out;
out.position = float4(ndc, 0.0, 1.0);
out.uv = float2(inst.uv_pos) + corner * float2(inst.uv_size);
out.tint = float4(inst.color);
out.local = pixel;
out.mask_center = float2(inst.mask_pos) + float2(inst.mask_size) * 0.5;
out.mask_half = float2(inst.mask_size) * 0.5;
out.mask_radius = float4(inst.mask_radius);
return out;";

const IMAGE_HELPERS: &str = "\
// Signed distance to a rounded box with an independent radius per corner
// (`radii` ordered left-top, right-top, right-bottom, left-bottom), negative
// inside; the radius convention is the Quad's.
static inline float rrect_sdf(float2 p, float2 center, float2 half_ext, float4 radii) {
    float2 d = p - center;
    float r = d.x < 0.0 ? (d.y < 0.0 ? radii.x : radii.w)
                        : (d.y < 0.0 ? radii.y : radii.z);
    float k = min(2.0 * r, min(half_ext.x, half_ext.y));
    float2 q = abs(d) - (half_ext - k);
    float2 mx = max(q, float2(0.0));
    return length(mx) + min(max(q.x, q.y), 0.0) - k;
}

// Device-pixel coverage factor: how many SDF units span one screen pixel at the
// current sampling position, inverted.
static inline float aa_factor(float2 p) {
    return 1.0 / length(float2(length(dfdx(p)), length(dfdy(p))));
}";

const IMAGE_FRAGMENT_BODY: &str = "\
// Texel is premultiplied linear (Viso texture convention). Scale it by the
// straight tint's premultiplied form: rgb by (tint.rgb * tint.a), a by
// tint.a — keeping the result premultiplied.
float4 texel = tex.sample(samp, in.uv);
float4 t = float4(in.tint.rgb * in.tint.a, in.tint.a);
// A rounded mask (a clip's composite, a rounded box's image) covers what lies
// inside it, antialiased over one device pixel; without one every sample is in.
// The derivative is taken outside the branch, which is not uniform.
float aa = aa_factor(in.local);
float4 r = in.mask_radius;
float cov = 1.0;
if (max(max(r.x, r.y), max(r.z, r.w)) > 0.0) {
    float d = rrect_sdf(in.local, in.mask_center, in.mask_half, r);
    cov = clamp(-d * aa, 0.0, 1.0);
}
return texel * (t * cov);";

const BLUR_VERTEX_BODY: &str = "\
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
float2 at = float2(u.axis_x) * pixel.x + float2(u.axis_y) * pixel.y + float2(u.shift);
float2 ndc = float2(at.x / vp.x * 2.0 - 1.0,
                    1.0 - at.y / vp.y * 2.0);

VOut out;
out.position = float4(ndc, 0.0, 1.0);
out.uv = float2(inst.uv_pos) + corner * float2(inst.uv_size);
out.dir = float2(inst.dir);
out.sigma = float(inst.sigma);
out.radius = float(inst.radius);
out.uv_bounds = float4(float2(inst.uv_pos),
                       float2(inst.uv_pos) + float2(inst.uv_size));
return out;";

const BLUR_FRAGMENT_BODY: &str = "\
// Separable 1D Gaussian tap loop along `dir` (normalized uv step). The source
// texel is premultiplied linear; weights are exp(-(i*i)/(2*sigma*sigma)),
// normalized by their sum. sigma<=0 degrades to a single center tap.
//
// Taps are clamped to the source sub-rect, inset by half a texel so the outer
// texel row/column is held rather than blended with whatever lies beyond it:
// a pooled render target is larger than the region written into it, so the
// sampler's own clamp-to-edge would hold cleared padding, not the content edge.
float sigma = in.sigma;
int R = (sigma > 0.0) ? int(in.radius) : 0;
float two_sigma_sq = 2.0 * sigma * sigma;
float2 half_texel = 0.5 / float2(float(tex.get_width()), float(tex.get_height()));
float2 lo = in.uv_bounds.xy + half_texel;
float2 hi = max(in.uv_bounds.zw - half_texel, lo);
float4 acc = float4(0.0);
float wsum = 0.0;
for (int i = -R; i <= R; ++i) {
    float fi = float(i);
    float w = (two_sigma_sq > 0.0) ? exp(-(fi * fi) / two_sigma_sq) : 1.0;
    float2 suv = clamp(in.uv + fi * in.dir, lo, hi);
    acc += w * tex.sample(samp, suv);
    wsum += w;
}
return (wsum > 0.0) ? acc / wsum : float4(0.0);";

const COLOR_TRANSFORM_VERTEX_BODY: &str = "\
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
float2 at = float2(u.axis_x) * pixel.x + float2(u.axis_y) * pixel.y + float2(u.shift);
float2 ndc = float2(at.x / vp.x * 2.0 - 1.0,
                    1.0 - at.y / vp.y * 2.0);

VOut out;
out.position = float4(ndc, 0.0, 1.0);
out.uv = float2(inst.uv_pos) + corner * float2(inst.uv_size);
out.row0 = float4(inst.row0);
out.row1 = float4(inst.row1);
out.row2 = float4(inst.row2);
out.row3 = float4(inst.row3);
out.offset = float4(inst.offset);
out.gamma = float(inst.gamma);
return out;";

const COLOR_TRANSFORM_FRAGMENT_BODY: &str = "\
// The source texel is premultiplied linear, but a color matrix is defined on
// straight RGBA (brightness must not depend on coverage), so unpremultiply
// first. Then map through the four output rows plus the constant column, clamp
// to the representable range, apply the optional gamma, and repremultiply.
// A fully transparent texel carries no color: it enters the matrix as zero, so
// it keeps whatever the constant column says (normally still transparent).
float4 texel = tex.sample(samp, in.uv);
float a = texel.a;
float4 src = (a > 0.0) ? float4(texel.rgb / a, a) : float4(0.0);

float4 dst = float4(dot(in.row0, src) + in.offset.r,
                    dot(in.row1, src) + in.offset.g,
                    dot(in.row2, src) + in.offset.b,
                    dot(in.row3, src) + in.offset.a);
dst = clamp(dst, 0.0, 1.0);
if (in.gamma != 1.0) {
    dst.rgb = pow(dst.rgb, float3(in.gamma));
}
return float4(dst.rgb * dst.a, dst.a);";

const OUTPUT_ENCODE_VERTEX_BODY: &str = "\
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
float2 at = float2(u.axis_x) * pixel.x + float2(u.axis_y) * pixel.y + float2(u.shift);
float2 ndc = float2(at.x / vp.x * 2.0 - 1.0,
                    1.0 - at.y / vp.y * 2.0);

VOut out;
out.position = float4(ndc, 0.0, 1.0);
out.uv = float2(inst.uv_pos) + corner * float2(inst.uv_size);
out.local = pixel;
out.dither = float(inst.dither);
return out;";

const OUTPUT_ENCODE_HELPERS: &str = "\
// An ordered offset in [-0.5, 0.5] from an integer-lattice hash of the pixel:
// exactly defined integer mixing, so the CPU rasterizer matches it bit for bit
// and a still frame encodes the same every time.
static inline float lattice(float2 p) {
    uint x = uint(int(floor(p.x)));
    uint y = uint(int(floor(p.y)));
    uint h = (x * 0x9E3779B9u) ^ (y * 0x85EBCA6Bu);
    h ^= h >> 15;
    h *= 0x2C1B3C6Du;
    h ^= h >> 12;
    h *= 0x297A2D39u;
    h ^= h >> 15;
    return float(h & 0x00FFFFFFu) / 16777215.0 - 0.5;
}";

const OUTPUT_ENCODE_FRAGMENT_BODY: &str = "\
// The working texel is premultiplied linear light; the surface stores the sRGB
// encoding of it, `enc(dec(c) * a)`, as an `*Srgb` target would have.
float4 texel = tex.sample(samp, in.uv);
float3 c = clamp(texel.rgb, 0.0, 1.0);
float3 lo = c * 12.92;
float3 hi = 1.055 * pow(c, float3(1.0 / 2.4)) - 0.055;
float3 enc = mix(hi, lo, step(c, float3(0.0031308)));
enc = clamp(enc + in.dither * lattice(in.local), 0.0, 1.0);
return float4(enc, texel.a);";

const MATERIAL_VERTEX_BODY: &str = "\
InstanceIn inst = instances[iid];

// Padded by 1px each side like every analytic shape, so the mask's AA ramp at
// the panel edge is covered. The uv of that ring lands outside the backdrop
// sub-rect; the fragment clamps it back, and its coverage is ~0 anyway.
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

float2 vp = float2(u.viewport);
float2 at = float2(u.axis_x) * pixel.x + float2(u.axis_y) * pixel.y + float2(u.shift);
float2 ndc = float2(at.x / vp.x * 2.0 - 1.0,
                    1.0 - at.y / vp.y * 2.0);

float2 uv_lo = float2(inst.uv_pos);
float2 uv_hi = uv_lo + float2(inst.uv_size);
float2 uv_pad = float2(inst.uv_size) / max(size, float2(1.0, 1.0));

VOut out;
out.position = float4(ndc, 0.0, 1.0);
out.uv = uv_lo - uv_pad + corner * (float2(inst.uv_size) + 2.0 * uv_pad);
out.uv_bounds = float4(uv_lo, uv_hi);
out.local = pixel;
out.half_size = size * 0.5;
out.center = pos + size * 0.5;
out.radius = float4(inst.radius);
out.row0 = float4(inst.row0);
out.row1 = float4(inst.row1);
out.row2 = float4(inst.row2);
out.row3 = float4(inst.row3);
out.offset = float4(inst.offset);
out.gamma = float(inst.gamma);
out.noise = float(inst.noise);
out.opacity = float(inst.opacity);
return out;";

const MATERIAL_HELPERS: &str = "\
// Signed distance to a rounded box with an independent radius per corner
// (`radii` ordered left-top, right-top, right-bottom, left-bottom), negative
// inside. `half_ext` is the box's half-extents. (Do not name it `half` — that is
// a reserved MSL type name, the 16-bit float.)
static inline float rrect_sdf(float2 p, float2 center, float2 half_ext, float4 radii) {
    float2 d = p - center;
    float r = d.x < 0.0 ? (d.y < 0.0 ? radii.x : radii.w)
                        : (d.y < 0.0 ? radii.y : radii.z);
    float k = min(2.0 * r, min(half_ext.x, half_ext.y));
    float2 q = abs(d) - (half_ext - k);
    float2 mx = max(q, float2(0.0));
    return length(mx) + min(max(q.x, q.y), 0.0) - k;
}

// Device-pixel coverage factor: how many SDF units span one screen pixel at the
// current sampling position, inverted.
static inline float aa_factor(float2 p) {
    return 1.0 / length(float2(length(dfdx(p)), length(dfdy(p))));
}

// Grain in [-0.5, 0.5] from an integer-lattice hash of the device pixel.
//
// Integer mixing rather than a trigonometric hash: unsigned wraparound multiply and shift
// are exactly defined, so the value matches the CPU rasterizer bit for bit and
// carries no time input — a static frosted surface therefore renders identically
// every frame, which is what keeps an idle screen from re-uploading.
static inline float grain(float2 p) {
    uint x = uint(int(floor(p.x)));
    uint y = uint(int(floor(p.y)));
    uint h = (x * 0x9E3779B9u) ^ (y * 0x85EBCA6Bu);
    h ^= h >> 15;
    h *= 0x2C1B3C6Du;
    h ^= h >> 12;
    h *= 0x297A2D39u;
    h ^= h >> 15;
    return float(h & 0x00FFFFFFu) / 16777215.0 - 0.5;
}";

const MATERIAL_FRAGMENT_BODY: &str = "\
// Taps are clamped to the backdrop's sub-rect, inset by half a texel: a pooled
// render target is larger than the region written into it, so the sampler's own
// clamp-to-edge would hold cleared padding rather than the content edge.
float2 half_texel = 0.5 / float2(float(tex.get_width()), float(tex.get_height()));
float2 lo = in.uv_bounds.xy + half_texel;
float2 hi = max(in.uv_bounds.zw - half_texel, lo);
float4 texel = tex.sample(samp, clamp(in.uv, lo, hi));

// Tint: the same straight-alpha affine matrix the fused color built-in applies,
// so brightness never depends on coverage.
float a = texel.a;
float4 src = (a > 0.0) ? float4(texel.rgb / a, a) : float4(0.0);
float4 dst = float4(dot(in.row0, src) + in.offset.r,
                    dot(in.row1, src) + in.offset.g,
                    dot(in.row2, src) + in.offset.b,
                    dot(in.row3, src) + in.offset.a);
dst = clamp(dst, 0.0, 1.0);
if (in.gamma != 1.0) {
    dst.rgb = pow(dst.rgb, float3(in.gamma));
}
if (in.noise > 0.0) {
    dst.rgb = clamp(dst.rgb + in.noise * grain(in.local), 0.0, 1.0);
}

// Material mask: the panel's own rounded rect, antialiased over one device pixel.
float d = rrect_sdf(in.local, in.center, in.half_size, in.radius);
float cov = clamp(-d * aa_factor(in.local), 0.0, 1.0);

float oa = dst.a * cov * in.opacity;
return float4(dst.rgb * oa, oa);";

const ADVANCED_BLEND_VERTEX_BODY: &str = "\
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
float2 at = float2(u.axis_x) * pixel.x + float2(u.axis_y) * pixel.y + float2(u.shift);
float2 ndc = float2(at.x / vp.x * 2.0 - 1.0,
                    1.0 - at.y / vp.y * 2.0);

VOut out;
out.position = float4(ndc, 0.0, 1.0);
out.uv = float2(inst.uv_pos) + corner * float2(inst.uv_size);
out.dst_uv = float2(inst.dst_uv_pos) + corner * float2(inst.dst_uv_size);
out.mode = inst.mode;
out.opacity = float(inst.opacity);
return out;";

const ADVANCED_BLEND_HELPERS: &str = "\
// The Porter-Duff coverage pair (fa, fb) for modes 0..12, applied to
// premultiplied source and destination: co = fa*Cs + fb*Cb, ao = fa*as + fb*ab.
// `Plus` (12) is (1, 1) and relies on the caller's clamp, which is also the
// safe default for an out-of-range mode.
static inline float2 porter_duff(uint mode, float sa, float da) {
    switch (mode) {
        case 0u:  return float2(0.0, 0.0);
        case 1u:  return float2(1.0, 0.0);
        case 2u:  return float2(0.0, 1.0);
        case 3u:  return float2(1.0, 1.0 - sa);
        case 4u:  return float2(1.0 - da, 1.0);
        case 5u:  return float2(da, 0.0);
        case 6u:  return float2(0.0, sa);
        case 7u:  return float2(1.0 - da, 0.0);
        case 8u:  return float2(0.0, 1.0 - sa);
        case 9u:  return float2(da, 1.0 - sa);
        case 10u: return float2(1.0 - da, sa);
        case 11u: return float2(1.0 - da, 1.0 - sa);
        default:  return float2(1.0, 1.0);
    }
}

// One channel of the separable blend functions, on straight (unpremultiplied)
// values. Screen and hard-light are factored out because overlay and soft-light
// are defined in terms of them.
static inline float blend_screen(float cb, float cs) {
    return cb + cs - cb * cs;
}

static inline float blend_hard_light(float cb, float cs) {
    return (cs <= 0.5) ? (cb * 2.0 * cs) : blend_screen(cb, 2.0 * cs - 1.0);
}

static inline float blend_soft_light(float cb, float cs) {
    float d = (cb <= 0.25) ? (((16.0 * cb - 12.0) * cb + 4.0) * cb) : sqrt(cb);
    return (cs <= 0.5) ? (cb - (1.0 - 2.0 * cs) * cb * (1.0 - cb))
                       : (cb + (2.0 * cs - 1.0) * (d - cb));
}

// B(cb, cs) for the separable artistic modes 13..23. Overlay is hard-light with
// its arguments swapped; the dodge/burn guards are the W3C limits, which keep the
// divisions finite at full coverage.
static inline float blend_separable(uint mode, float cb, float cs) {
    switch (mode) {
        case 13u: return cb * cs;
        case 14u: return blend_screen(cb, cs);
        case 15u: return blend_hard_light(cs, cb);
        case 16u: return min(cb, cs);
        case 17u: return max(cb, cs);
        case 18u: return (cb <= 0.0) ? 0.0
                       : (cs >= 1.0) ? 1.0
                       : min(1.0, cb / (1.0 - cs));
        case 19u: return (cb >= 1.0) ? 1.0
                       : (cs <= 0.0) ? 0.0
                       : 1.0 - min(1.0, (1.0 - cb) / cs);
        case 20u: return blend_hard_light(cb, cs);
        case 21u: return blend_soft_light(cb, cs);
        case 22u: return fabs(cb - cs);
        default:  return cb + cs - 2.0 * cb * cs;
    }
}

// The non-separable HSL helpers, verbatim from the W3C compositing model:
// luminosity is the Rec.601 luma, and clipping keeps a relit color in gamut by
// scaling it about its own luminosity rather than clamping per channel.
static inline float blend_lum(float3 c) {
    return dot(c, float3(0.3, 0.59, 0.11));
}

static inline float3 blend_clip_color(float3 c) {
    float l = blend_lum(c);
    float n = min(c.r, min(c.g, c.b));
    float x = max(c.r, max(c.g, c.b));
    if (n < 0.0) {
        c = l + (c - l) * l / max(l - n, 1e-6);
    }
    if (x > 1.0) {
        c = l + (c - l) * (1.0 - l) / max(x - l, 1e-6);
    }
    return c;
}

static inline float3 blend_set_lum(float3 c, float l) {
    return blend_clip_color(c + (l - blend_lum(c)));
}

static inline float blend_sat(float3 c) {
    return max(c.r, max(c.g, c.b)) - min(c.r, min(c.g, c.b));
}

static inline float3 blend_set_sat(float3 c, float s) {
    float n = min(c.r, min(c.g, c.b));
    float x = max(c.r, max(c.g, c.b));
    return (x > n) ? ((c - n) * s / (x - n)) : float3(0.0);
}

// B(cb, cs) for the non-separable modes 24..27, which mix a hue/saturation/
// luminosity component of one operand into the other.
static inline float3 blend_nonseparable(uint mode, float3 cb, float3 cs) {
    switch (mode) {
        case 24u: return blend_set_lum(blend_set_sat(cs, blend_sat(cb)), blend_lum(cb));
        case 25u: return blend_set_lum(blend_set_sat(cb, blend_sat(cs)), blend_lum(cb));
        case 26u: return blend_set_lum(cs, blend_lum(cb));
        default:  return blend_set_lum(cb, blend_lum(cs));
    }
}";

const ADVANCED_BLEND_FRAGMENT_BODY: &str = "\
// Source is the isolated layer, destination the bounded snapshot of what is
// behind it. Both are premultiplied; the layer opacity scales the whole source
// (a premultiplied scale is exactly a coverage scale). The result is final —
// the pipeline writes it with `Replace`, so the destination contributes here and
// nowhere else.
float4 s = tex.sample(samp, in.uv) * in.opacity;
float4 d = dst_tex.sample(samp, in.dst_uv);
float sa = s.a;
float da = d.a;

float4 result;
if (in.mode <= 12u) {
    // Porter-Duff: linear in the premultiplied operands, no unpremultiply.
    float2 f = porter_duff(in.mode, sa, da);
    result = float4(f.x * s.rgb + f.y * d.rgb, f.x * sa + f.y * da);
} else {
    // The advanced modes are defined on straight color, with the union of the
    // two coverages as the output alpha:
    //   co = as*(1-ab)*cs + as*ab*B(cb,cs) + (1-as)*ab*cb
    float3 cs = (sa > 0.0) ? (s.rgb / sa) : float3(0.0);
    float3 cb = (da > 0.0) ? (d.rgb / da) : float3(0.0);
    float3 b = (in.mode <= 23u)
        ? float3(blend_separable(in.mode, cb.r, cs.r),
                 blend_separable(in.mode, cb.g, cs.g),
                 blend_separable(in.mode, cb.b, cs.b))
        : blend_nonseparable(in.mode, cb, cs);
    float ao = sa + da - sa * da;
    float3 co = sa * (1.0 - da) * cs + sa * da * b + (1.0 - sa) * da * cb;
    result = float4(co, ao);
}
return clamp(result, 0.0, 1.0);";

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
float2 at = float2(u.axis_x) * pixel.x + float2(u.axis_y) * pixel.y + float2(u.shift);
float2 ndc = float2(at.x / vp.x * 2.0 - 1.0,
                    1.0 - at.y / vp.y * 2.0);

VOut out;
out.position = float4(ndc, 0.0, 1.0);
out.uv = float2(inst.uv_pos) + corner * float2(inst.uv_size);
out.color = float4(inst.color);
return out;";

const INK_HELPERS: &str = "\
// An sRGB-encoded value in linear light.
static inline float srgb_decode(float v) {
    return v <= 0.04045 ? v / 12.92 : pow((v + 0.055) / 1.055, 2.4);
}

// Glyph coverage `cov` adjusted by the luminance of the linear `ink`, so a
// glyph blended in linear light keeps the weight it has blended in encoded
// space: exactly so for a black ink on white and a white one on black.
static inline float ink_coverage(float cov, float3 ink) {
    float l = clamp(dot(ink, float3(0.2126, 0.7152, 0.0722)), 0.0, 1.0);
    return (1.0 - l) * (1.0 - srgb_decode(1.0 - cov)) + l * srgb_decode(cov);
}";

const GLYPHRUN_FRAGMENT_BODY: &str = "\
// Single-channel A8 coverage sampled directly: the atlas texel's red channel
// is exact per-pixel coverage. Modulate the run color by it, premultiplied.
float cov = ink_coverage(tex.sample(samp, in.uv).r, in.color.rgb);
float a = in.color.a * cov;
return float4(in.color.rgb * a, a);";

const MTSDF_VERTEX_BODY: &str = "\
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
float2 at = float2(u.axis_x) * pixel.x + float2(u.axis_y) * pixel.y + float2(u.shift);
float2 ndc = float2(at.x / vp.x * 2.0 - 1.0,
                    1.0 - at.y / vp.y * 2.0);

VOut out;
out.position = float4(ndc, 0.0, 1.0);
out.uv = float2(inst.uv_pos) + corner * float2(inst.uv_size);
out.color = float4(inst.color);
return out;";

const MTSDF_FRAGMENT_BODY: &str = "\
// The median of the three channels reconstructs the glyph's edge, including a
// sharp corner where a single channel would round it. Alpha carries the true
// signed distance: where the two disagree on which side of the edge this texel
// is, the median is a multi-channel interpolation artifact and the true distance
// wins.
float4 s = tex.sample(samp, in.uv);
float md = max(min(s.r, s.g), min(max(s.r, s.g), s.b));
float sd = ((md > 0.5) != (s.a > 0.5)) ? s.a : md;

// Distances are stored in field texels. How many of them one device pixel
// spans, read off the uv's screen derivatives, turns them into pixels, so the
// edge stays one pixel wide at every scale, rotation and display density.
float distance_range = 4.0;
float2 texels = float2(float(tex.get_width()), float(tex.get_height()));
float per_px = 0.5 * (length(dfdx(in.uv) * texels) + length(dfdy(in.uv) * texels));
float px_range = distance_range / max(per_px, 1e-6);
float cov = ink_coverage(clamp((sd - 0.5) * px_range + 0.5, 0.0, 1.0), in.color.rgb);
float a = in.color.a * cov;
return float4(in.color.rgb * a, a);";

const MESH_VERTEX_BODY: &str = "\
VertexIn v = verts[vid];

float2 pixel = float2(v.pos);
float2 vp = float2(u.viewport);
float2 ax = float2(u.axis_x);
float2 ay = float2(u.axis_y);
float2 ex = float2(v.extrude);
float2 at = ax * pixel.x + ay * pixel.y + float2(u.shift)
    + ax * (ex.x / max(length(ax), 0.000001)) + ay * (ex.y / max(length(ay), 0.000001));
float2 ndc = float2(at.x / vp.x * 2.0 - 1.0,
                    1.0 - at.y / vp.y * 2.0);

VOut out;
out.position = float4(ndc, 0.0, 1.0);
out.color = float4(v.color);
out.edge = v.edge;
return out;";

const MESH_FRAGMENT_BODY: &str = "\
float cov = clamp(in.edge, 0.0, 1.0);
float a = in.color.a * cov;
return float4(in.color.rgb * a, a);";

/// The opening of a mesh's vertex body: its vertex and instance, and the
/// vertex's position, normal and tangent in the mesh's space.
macro_rules! scene_vertex_head {
    () => {
        "\
VertexIn v = verts[vid];
InstanceIn inst = instances[iid];
float3 p = float3(v.position);
float3 n = float3(v.normal);
float4 t = float4(v.tangent);
"
    };
}

/// The opening of a skinned mesh's vertex body: the vertex moved by its
/// joints, the weighted sum of their affines (three rows each), its normal
/// through the sum's cofactor, turned back out where the sum mirrors.
macro_rules! scene_skinned_head {
    () => {
        concat!(
            scene_vertex_head!(),
            "\
uint2 jp = v.joints;
uint2 wp = v.weights;
float4 w = float4(float(wp.x & 65535u), float(wp.x >> 16u), float(wp.y & 65535u),
    float(wp.y >> 16u)) / 65535.0;
float wsum = w.x + w.y + w.z + w.w;
if (wsum > 0.0) {
    w = w / wsum;
} else {
    w = float4(1.0, 0.0, 0.0, 0.0);
}
int j0 = int(min(jp.x & 65535u, 255u)) * 3;
int j1 = int(min(jp.x >> 16u, 255u)) * 3;
int j2 = int(min(jp.y & 65535u, 255u)) * 3;
int j3 = int(min(jp.y >> 16u, 255u)) * 3;
float4 s0 = float4(skin.joints[j0]) * w.x + float4(skin.joints[j1]) * w.y
    + float4(skin.joints[j2]) * w.z + float4(skin.joints[j3]) * w.w;
float4 s1 = float4(skin.joints[j0 + 1]) * w.x + float4(skin.joints[j1 + 1]) * w.y
    + float4(skin.joints[j2 + 1]) * w.z + float4(skin.joints[j3 + 1]) * w.w;
float4 s2 = float4(skin.joints[j0 + 2]) * w.x + float4(skin.joints[j1 + 2]) * w.y
    + float4(skin.joints[j2 + 2]) * w.z + float4(skin.joints[j3 + 2]) * w.w;
float3 a0 = float3(s0.x, s1.x, s2.x);
float3 a1 = float3(s0.y, s1.y, s2.y);
float3 a2 = float3(s0.z, s1.z, s2.z);
float sk = dot(a0, cross(a1, a2)) < 0.0 ? -1.0 : 1.0;
p = float3(dot(s0.xyz, p) + s0.w, dot(s1.xyz, p) + s1.w, dot(s2.xyz, p) + s2.w);
n = (cross(a1, a2) * n.x + cross(a2, a0) * n.y + cross(a0, a1) * n.z) * sk;
t = float4(a0 * t.x + a1 * t.y + a2 * t.z, t.w * sk);
"
        )
    };
}

/// The rest of the mesh program's vertex body, from the vertex's `p`, `n`
/// and `t`.
macro_rules! scene_mesh_tail {
    () => {
        "\
// The vertex in the world: the instance's affine, as three rows.
float4 r0 = float4(inst.row0);
float4 r1 = float4(inst.row1);
float4 r2 = float4(inst.row2);
float3 world = float3(dot(r0.xyz, p) + r0.w, dot(r1.xyz, p) + r1.w, dot(r2.xyz, p) + r2.w);

// The normal through the cofactor of the linear part, so a non-uniform scale
// keeps it perpendicular to the surface, and turned back out by a mirroring
// instance, whose bitangents flip; the tangent through the linear part.
float3 c0 = float3(r0.x, r1.x, r2.x);
float3 c1 = float3(r0.y, r1.y, r2.y);
float3 c2 = float3(r0.z, r1.z, r2.z);
float side = dot(c0, cross(c1, c2)) < 0.0 ? -1.0 : 1.0;

VOut out;
out.position = float4(u.view_proj0) * world.x + float4(u.view_proj1) * world.y
    + float4(u.view_proj2) * world.z + float4(u.view_proj3);
out.world = world;
out.normal = (cross(c1, c2) * n.x + cross(c2, c0) * n.y + cross(c0, c1) * n.z) * side;
out.tangent = float4(c0 * t.x + c1 * t.y + c2 * t.z, t.w * side);
out.uv = float2(v.uv);
out.color = float4(inst.color);
"
    };
}

/// The rest of the depth program's vertex body, from the vertex's `p`.
macro_rules! scene_depth_tail {
    () => {
        "\
float4 r0 = float4(inst.row0);
float4 r1 = float4(inst.row1);
float4 r2 = float4(inst.row2);
float3 world = float3(dot(r0.xyz, p) + r0.w, dot(r1.xyz, p) + r1.w, dot(r2.xyz, p) + r2.w);
float4 clip = float4(u.view_proj0) * world.x + float4(u.view_proj1) * world.y
    + float4(u.view_proj2) * world.z + float4(u.view_proj3);

VOut out;
out.position = clip;
out.uv = float2(v.uv);
// Reversed depth: the clip depth falls away from the light.
out.light_depth = 1.0 - clip.z / clip.w;
out.tint_alpha = float4(inst.color).w;
"
    };
}

/// The mesh program's varyings of a vertex's extra: its second texture
/// coordinate set and its color, unpacked.
macro_rules! scene_mesh_extra {
    () => {
        "\
out.uv1 = float2(v.uv1);
uint2 cp = v.color;
out.vertex_color = float4(float(cp.x & 65535u), float(cp.x >> 16u), float(cp.y & 65535u),
    float(cp.y >> 16u)) / 65535.0;
"
    };
}

/// The depth program's varyings of a vertex's extra: its second texture
/// coordinate set and its color's alpha.
macro_rules! scene_depth_extra {
    () => {
        "\
out.uv1 = float2(v.uv1);
out.vertex_alpha = float(v.color.y >> 16u) / 65535.0;
"
    };
}

const SCENE_VERTEX_BODY: &str = concat!(scene_vertex_head!(), scene_mesh_tail!(), "return out;");
const SCENE_SKINNED_VERTEX_BODY: &str =
    concat!(scene_skinned_head!(), scene_mesh_tail!(), "return out;");
const SCENE_DEPTH_VERTEX_BODY: &str =
    concat!(scene_vertex_head!(), scene_depth_tail!(), "return out;");
const SCENE_SKINNED_DEPTH_VERTEX_BODY: &str =
    concat!(scene_skinned_head!(), scene_depth_tail!(), "return out;");
const SCENE_EXTRA_VERTEX_BODY: &str = concat!(
    scene_vertex_head!(),
    scene_mesh_tail!(),
    scene_mesh_extra!(),
    "return out;"
);
const SCENE_EXTRA_SKINNED_VERTEX_BODY: &str = concat!(
    scene_skinned_head!(),
    scene_mesh_tail!(),
    scene_mesh_extra!(),
    "return out;"
);
const SCENE_EXTRA_DEPTH_VERTEX_BODY: &str = concat!(
    scene_vertex_head!(),
    scene_depth_tail!(),
    scene_depth_extra!(),
    "return out;"
);
const SCENE_EXTRA_SKINNED_DEPTH_VERTEX_BODY: &str = concat!(
    scene_skinned_head!(),
    scene_depth_tail!(),
    scene_depth_extra!(),
    "return out;"
);

/// A screen program's vertex body: the vertex's position is its clip-space
/// `x` and `y`, at the far plane; its instance is unread.
const SCENE_SCREEN_VERTEX_BODY: &str = "\
VertexIn v = verts[vid];
InstanceIn inst = instances[iid];
float3 p = float3(v.position);
VOut out;
out.position = float4(p.x, p.y, 0.0, 1.0);
out.ndc = float2(p.x, p.y);
return out;";

const SCENE_SKY_FRAGMENT_BODY: &str = "\
// The view ray: from the near plane toward twice as far in depth.
float2 ndc = in.ndc;
float4 i0 = float4(sky.inv_view_proj0);
float4 i1 = float4(sky.inv_view_proj1);
float4 i2 = float4(sky.inv_view_proj2);
float4 i3 = float4(sky.inv_view_proj3);
float4 pn = i0 * ndc.x + i1 * ndc.y + i2 * 1.0 + i3;
float4 pf = i0 * ndc.x + i1 * ndc.y + i2 * 0.5 + i3;
float3 d = normalize(pf.xyz / pf.w - pn.xyz / pn.w);

// The Perez functions of the luminance and the two chromaticities.
float altitude = d.y;
float cos_theta = max(altitude, 0.01);
float cos_gamma = clamp(dot(d, float4(sky.sun_model).xyz), -1.0, 1.0);
float gamma = acos(cos_gamma);
float4 py = float4(sky.perez_y);
float4 px = float4(sky.perez_x);
float4 pc = float4(sky.perez_yc);
float4 pe = float4(sky.perez_e);
float4 zen = float4(sky.zenith);
float big_y = zen.x * ((1.0 + py.x * exp(py.y / cos_theta))
    * (1.0 + py.z * exp(py.w * gamma) + pe.x * cos_gamma * cos_gamma)) * pe.w;
float cx = zen.y * ((1.0 + px.x * exp(px.y / cos_theta))
    * (1.0 + px.z * exp(px.w * gamma) + pe.y * cos_gamma * cos_gamma));
float cy = max(zen.z * ((1.0 + pc.x * exp(pc.y / cos_theta))
    * (1.0 + pc.z * exp(pc.w * gamma) + pe.z * cos_gamma * cos_gamma)), 0.0001);
float below = clamp(-altitude * 3.0, 0.0, 1.0);

// xyY to XYZ to linear sRGB.
float big_x = cx * (big_y / cy);
float big_z = (1.0 - cx - cy) * (big_y / cy);
float4 blend = float4(sky.blend);
float day_weight = (1.0 - below * zen.w) * blend.x;
float3 day = float3(max(3.2406 * big_x - 1.5372 * big_y - 0.4986 * big_z, 0.0),
    max(-0.9689 * big_x + 1.8758 * big_y + 0.0415 * big_z, 0.0),
    max(0.0557 * big_x - 0.204 * big_y + 1.057 * big_z, 0.0)) * day_weight;

// The night dome as the day fades, and the twilight's glow.
float height = sqrt(max(altitude, 0.0));
float night_weight = (1.0 - blend.x) * (1.0 - 0.55 * below);
float4 sun = float4(sky.sun);
float facing = dot(d, sun.xyz);
float sun_gamma = acos(clamp(facing, -1.0, 1.0));
float glow = exp(-abs(altitude) * 7.0) * exp(-sun_gamma * 3.5);
float3 night = float3(0.0035, 0.0055, 0.011)
    + (float3(0.001, 0.0018, 0.0045) - float3(0.0035, 0.0055, 0.011)) * height;
float3 dusk = (float3(0.01, 0.018, 0.045) * (0.35 + 0.65 * height)
    + float3(0.1, 0.032, 0.006) * glow) * blend.y;
float3 c = day + night * night_weight + dusk;

// The sun's disc, its rim smoothed, by the angle from its centre.
if (sun.w > 0.0 && facing > 0.0) {
    float angle = length(cross(d, sun.xyz));
    float t = clamp((angle - sun.w * 0.85) / (sun.w * 1.15 - sun.w * 0.85), 0.0, 1.0);
    c = c + float4(sky.sun_radiance).xyz * (1.0 - t * t * (3.0 - 2.0 * t));
}
float3 m = tonemap(c * float4(u.camera).w);
return float4(clamp(m, 0.0, 1.0), 1.0);";

const SCENE_AO_HELPERS: &str = "\
// The depth a packed texel holds: 24 bits, the most significant byte in red.
static inline float unpack_depth(float4 t) {
    float3 b = floor(t.xyz * 255.0 + 0.5);
    return (b.x * 65536.0 + b.y * 256.0 + b.z) / 16777215.0;
}

// The world point at clip-space `ndc` and depth `z`, by the four columns of
// the inverse view-projection.
static inline float3 unproject(float4 i0, float4 i1, float4 i2, float4 i3, float2 ndc, float z) {
    float4 p = i0 * ndc.x + i1 * ndc.y + i2 * z + i3;
    return p.xyz / p.w;
}

// Smoothstep over [0, 1].
static inline float ramp(float t) {
    float c = clamp(t, 0.0, 1.0);
    return c * c * (3.0 - 2.0 * c);
}";

const SCENE_AO_FRAGMENT_BODY: &str = "\
float4 i0 = float4(ao.inv_view_proj0);
float4 i1 = float4(ao.inv_view_proj1);
float4 i2 = float4(ao.inv_view_proj2);
float4 i3 = float4(ao.inv_view_proj3);
float3 eye = float4(u.camera).xyz;
float2 ndc = in.ndc;
float2 uv = float2(ndc.x * 0.5 + 0.5, 0.5 - ndc.y * 0.5);
float z = 1.0 - unpack_depth(depth_tex.sample(point_samp, uv, gradient2d(float2(0.0), float2(0.0))));
if (z <= 0.0) {
    return float4(1.0, 0.0, 0.0, 1.0);
}
float3 p = unproject(i0, i1, i2, i3, ndc, z);
float d = length(p - eye);

// The neighbours' points, each axis's slope from the side nearer in depth.
float4 texel = float4(ao.texel);
float2 ul = uv + float2(-texel.x, 0.0);
float2 ur = uv + float2(texel.x, 0.0);
float2 uu = uv + float2(0.0, -texel.y);
float2 ud = uv + float2(0.0, texel.y);
float zl = 1.0 - unpack_depth(depth_tex.sample(point_samp, ul, gradient2d(float2(0.0), float2(0.0))));
float zr = 1.0 - unpack_depth(depth_tex.sample(point_samp, ur, gradient2d(float2(0.0), float2(0.0))));
float zu = 1.0 - unpack_depth(depth_tex.sample(point_samp, uu, gradient2d(float2(0.0), float2(0.0))));
float zd = 1.0 - unpack_depth(depth_tex.sample(point_samp, ud, gradient2d(float2(0.0), float2(0.0))));
float3 pl = float3(0.0);
float3 pr = float3(0.0);
float3 pu = float3(0.0);
float3 pd = float3(0.0);
float dl = 0.0;
float dr = 0.0;
float du = 0.0;
float dd = 0.0;
if (zl > 0.0) {
    pl = unproject(i0, i1, i2, i3, float2(ul.x * 2.0 - 1.0, 1.0 - ul.y * 2.0), zl);
    dl = length(pl - eye);
}
if (zr > 0.0) {
    pr = unproject(i0, i1, i2, i3, float2(ur.x * 2.0 - 1.0, 1.0 - ur.y * 2.0), zr);
    dr = length(pr - eye);
}
if (zu > 0.0) {
    pu = unproject(i0, i1, i2, i3, float2(uu.x * 2.0 - 1.0, 1.0 - uu.y * 2.0), zu);
    du = length(pu - eye);
}
if (zd > 0.0) {
    pd = unproject(i0, i1, i2, i3, float2(ud.x * 2.0 - 1.0, 1.0 - ud.y * 2.0), zd);
    dd = length(pd - eye);
}
float3 ddx = pr - p;
if (dr <= 0.0 || (dl > 0.0 && abs(dl - d) < abs(dr - d))) {
    ddx = p - pl;
}
float3 ddy = pd - p;
if (dd <= 0.0 || (du > 0.0 && abs(du - d) < abs(dd - d))) {
    ddy = p - pu;
}
float3 to_eye = eye - p;
float3 n = cross(ddx, ddy);
if (length(n) < 0.000000000001) {
    n = to_eye;
}
n = normalize(n);
if (dot(n, to_eye) < 0.0) {
    n = -n;
}
float3 up = float3(0.0, 1.0, 0.0);
if (abs(n.y) > 0.9) {
    up = float3(1.0, 0.0, 0.0);
}
float3 t = normalize(cross(up, n));
float3 b = cross(n, t);

// Interleaved gradient noise of the texel turns the spiral.
float px = floor(uv.x * texel.z);
float py = floor(uv.y * texel.w);
float inner = 0.06711056 * px + 0.00583715 * py;
float outer = 52.9829189 * (inner - floor(inner));
float rot = (outer - floor(outer)) * 6.2831853;
float4 params = float4(ao.params);
float radius = params.x;
float bias = params.y + d * params.z;
float occ = 0.0;
for (int k = 0; k < 12; ++k) {
    float kf = float(k);
    float fi = (kf + 0.5) / 12.0;
    float ang = kf * 2.3999632 + rot;
    float zt = 0.18 + 0.74 * fi;
    float r = sqrt(max(1.0 - zt * zt, 0.0));
    float reach = (0.2 + 0.8 * fi * fi) * radius;
    float3 s = p + (t * (cos(ang) * r) + b * (sin(ang) * r) + n * zt) * reach;
    float4 c = float4(u.view_proj0) * s.x + float4(u.view_proj1) * s.y
        + float4(u.view_proj2) * s.z + float4(u.view_proj3);
    if (c.w > 0.0) {
        float2 su = float2(c.x / c.w * 0.5 + 0.5, 0.5 - c.y / c.w * 0.5);
        if (su.x > 0.0 && su.x < 1.0 && su.y > 0.0 && su.y < 1.0) {
            float sz = 1.0 - unpack_depth(depth_tex.sample(point_samp, su, gradient2d(float2(0.0), float2(0.0))));
            if (sz > 0.0) {
                float3 q = unproject(i0, i1, i2, i3, float2(su.x * 2.0 - 1.0, 1.0 - su.y * 2.0), sz);
                float sd = length(q - eye);
                float ahead = length(s - eye) - sd;
                float range = 1.0 - ramp((abs(d - sd) - radius) / (radius * 2.0 - radius));
                occ = occ + ramp((ahead - bias) / (bias * 2.0 - bias)) * range;
            }
        }
    }
}

// The occlusion, and the distance in sixteen bits for the blur.
float a = clamp(1.0 - occ / 12.0, 0.0, 1.0);
float e = clamp(log2(1.0 + d) / 16.0, 0.0, 1.0);
float v = floor(e * 65535.0);
float hi = floor(v / 256.0);
float lo = v - hi * 256.0;
return float4(a, hi / 255.0, lo / 255.0, 1.0);";

const SCENE_AO_BLUR_FRAGMENT_BODY: &str = "\
float2 ndc = in.ndc;
float2 uv = float2(ndc.x * 0.5 + 0.5, 0.5 - ndc.y * 0.5);
float4 c = depth_tex.sample(point_samp, uv, gradient2d(float2(0.0), float2(0.0)));
float d = exp2((floor(c.y * 255.0 + 0.5) * 256.0 + floor(c.z * 255.0 + 0.5)) / 65535.0 * 16.0) - 1.0;
if (d <= 0.0) {
    return float4(c.x, c.y, c.z, 1.0);
}
float sigma = 0.03 * d + 0.02;
float4 st = float4(ao.step);
float sum = 0.0;
float weight = 0.0;
for (int k = 0; k < 9; ++k) {
    float kf = float(k) - 4.0;
    float4 s = depth_tex.sample(point_samp, uv + st.xy * kf, gradient2d(float2(0.0), float2(0.0)));
    float sd = exp2((floor(s.y * 255.0 + 0.5) * 256.0 + floor(s.z * 255.0 + 0.5)) / 65535.0 * 16.0) - 1.0;
    float dz = (sd - d) / sigma;
    if (sd > 0.0) {
        float w = exp(-kf * kf * 0.1033) * exp(-dz * dz);
        sum = sum + s.x * w;
        weight = weight + w;
    }
}
return float4(sum / max(weight, 0.0001), c.y, c.z, 1.0);";

const SCENE_HELPERS: &str = "\
// The first texture coordinate set, or the second where the material's
// `bits` set `flag`.
static inline float2 uv_set(uint bits, uint flag, float2 first, float2 second) {
    return (bits & flag) != 0u ? second : first;
}

// The GGX distribution of microfacet normals at `nh`, for roughness squared
// squared `a2`.
static inline float ggx(float nh, float a2) {
    float d = nh * nh * (a2 - 1.0) + 1.0;
    return a2 / (M_PI_F * d * d);
}

// The height-correlated Smith visibility of the microfacets.
static inline float smith(float nv, float nl, float a2) {
    float gv = nl * sqrt(nv * nv * (1.0 - a2) + a2);
    float gl = nv * sqrt(nl * nl * (1.0 - a2) + a2);
    return 0.5 / (gv + gl);
}

// The scale and bias of `f0` an environment's reflection is lit by: an
// analytic fit of the split-sum integral.
static inline float2 env_brdf(float roughness, float nv) {
    float4 r = float4(-1.0, -0.0275, -0.572, 0.022) * roughness + float4(1.0, 0.0425, 1.04, -0.04);
    float a = min(r.x * r.x, exp2(-9.28 * nv)) * r.x + r.y;
    return float2(-1.04 * a + r.z, 1.04 * a + r.w);
}

// Where direction `d` falls on an equirectangular map.
static inline float2 env_direction(float3 d) {
    return float2(atan2(d.x, -d.z) * (0.5 / M_PI_F) + 0.5, acos(clamp(d.y, -1.0, 1.0)) / M_PI_F);
}

// The environment atlas coordinate of `uv` on roughness level `level`'s map,
// half a texel inside it: level `k` is 256 >> k by 128 >> k texels at row
// 256 - (256 >> k) of the 256-texel-square atlas.
static inline float2 env_atlas(float2 uv, float level) {
    float s = exp2(-level);
    float w = 256.0 * s;
    float h = 128.0 * s;
    float x = clamp(uv.x * w, 0.5, w - 0.5);
    float y = 256.0 - 256.0 * s + clamp(uv.y * h, 0.5, h - 0.5);
    return float2(x, y) / 256.0;
}

// The depth a shadow atlas texel holds: 24 bits, the most significant byte
// in red.
static inline float unpack_depth(float4 t) {
    float3 b = floor(t.xyz * 255.0 + 0.5);
    return (b.x * 65536.0 + b.y * 256.0 + b.z) / 16777215.0;
}

// `p` in a cascade's space, by its three rows: x and y over its tile in
// [-1, 1], and the depth from the light.
static inline float3 cascade_point(float4 rx, float4 ry, float4 rz, float3 p) {
    return float3(dot(rx.xyz, p) + rx.w, dot(ry.xyz, p) + ry.w, dot(rz.xyz, p) + rz.w);
}

// Whether cascade point `q` lies inside its cascade.
static inline bool in_cascade(float3 q) {
    return max(abs(q.x), abs(q.y)) < 1.0 && q.z >= 0.0 && q.z <= 1.0;
}

// Linear light into [0, 1], hues kept: the Khronos PBR Neutral curve.
static inline float3 tonemap(float3 c) {
    float x = min(c.x, min(c.y, c.z));
    float offset = x < 0.08 ? x - 6.25 * x * x : 0.04;
    float3 d = c - offset;
    float peak = max(d.x, max(d.y, d.z));
    if (peak < 0.76) {
        return d;
    }
    float e = 0.24;
    float new_peak = 1.0 - e * e / (peak + e - 0.76);
    d = d * (new_peak / peak);
    float g = 1.0 - 1.0 / (0.15 * (peak - new_peak) + 1.0);
    return d + (float3(new_peak) - d) * g;
}";

/// The mesh program's fragment over vertices without extras: the first
/// texture coordinate set in the second's place, a white vertex color.
macro_rules! scene_fragment_plain {
    () => {
        "\
float2 uv1 = in.uv;
float4 vertex_color = float4(1.0);
"
    };
}

/// The mesh program's fragment over vertices with extras.
macro_rules! scene_fragment_extra {
    () => {
        "\
float2 uv1 = in.uv1;
float4 vertex_color = in.vertex_color;
"
    };
}

/// The derivatives every sample selects its detail by, taken before any
/// fragment of the quad leaves, and the material's mode.
macro_rules! scene_fragment_head {
    () => {
        "\
// The derivatives every sample selects its detail by, taken before any
// fragment of the quad leaves.
float2 uv_dx = dfdx(in.uv);
float2 uv_dy = dfdy(in.uv);
float2 uv1_dx = dfdx(uv1);
float2 uv1_dy = dfdy(uv1);
float3 n_geom = normalize(in.normal);
float3 n_dx = dfdx(n_geom);
float3 n_dy = dfdy(n_geom);

float4 mode = float4(material.mode);
uint bits = uint(mode.w);
"
    };
}

/// A skinned fragment of a one-sided material on the back of its triangle
/// discarded: its outward normal points away from the side of the
/// triangle's plane the camera is on. Its joints may mirror some triangles
/// and not others, so no pipeline culls them by their winding.
macro_rules! scene_fragment_face_test {
    () => {
        "\
float3 world_dx = dfdx(in.world);
float3 world_dy = dfdy(in.world);
if ((bits & 2u) == 0u) {
    float3 face = cross(world_dx, world_dy);
    if (dot(face, float4(u.camera).xyz - in.world) < 0.0) {
        face = -face;
    }
    if (dot(in.normal, face) < 0.0) {
        discard_fragment();
        return float4(0.0);
    }
}
"
    };
}

/// The rest of the mesh program's fragment.
macro_rules! scene_fragment_rest {
    () => {
        "\
float4 base = base_tex.sample(material_samp, uv_set(bits, 4u, in.uv, uv1),
    gradient2d(uv_set(bits, 4u, uv_dx, uv1_dx), uv_set(bits, 4u, uv_dy, uv1_dy)))
    * float4(material.base_color) * vertex_color;
float tint_a = in.color.a;
if (tint_a <= 0.0) {
    discard_fragment();
    return float4(0.0);
}
base = float4(base.xyz * (in.color.xyz / tint_a), base.w * tint_a);
float a = tint_a;
if (mode.y > 1.5) {
    a = base.w;
} else if (mode.y > 0.5) {
    if (base.w < mode.z) {
        discard_fragment();
        return float4(0.0);
    }
}

float4 camera = float4(u.camera);
float3 to_eye = camera.xyz - in.world;
float distance = length(to_eye);
float3 c = base.xyz;
if (mode.x > 1.5) {
    // A sprite: a disc across the quad, fading smoothly to its rim.
    float2 d = in.uv * 2.0 - 1.0;
    float t = clamp(1.0 - length(d), 0.0, 1.0);
    a = a * (t * t * (3.0 - 2.0 * t));
    if (a <= 0.0) {
        discard_fragment();
        return float4(0.0);
    }
} else if (mode.x < 0.5) {
    float3 v = to_eye / max(distance, 0.000001);
    float side = 1.0;
    if ((bits & 2u) != 0u && dot(n_geom, v) < 0.0) {
        side = -1.0;
    }
    float3 n = n_geom * side;
    float4 params = float4(material.params);
    if ((bits & 1u) != 0u) {
        float4 m = normal_tex.sample(material_samp, uv_set(bits, 16u, in.uv, uv1),
            gradient2d(uv_set(bits, 16u, uv_dx, uv1_dx), uv_set(bits, 16u, uv_dy, uv1_dy)));
        float3 tn = float3((m.x * 2.0 - 1.0) * params.z, (m.y * 2.0 - 1.0) * params.z, m.z * 2.0 - 1.0);
        float3 t = in.tangent.xyz * side;
        t = t - n * dot(n, t);
        float tl = length(t);
        if (tl > 0.000001) {
            t = t / tl;
            float3 b = cross(n, t) * (in.tangent.w * side);
            n = normalize(t * tn.x + b * tn.y + n * tn.z);
        }
    }
    float4 mr = mr_tex.sample(material_samp, uv_set(bits, 8u, in.uv, uv1),
        gradient2d(uv_set(bits, 8u, uv_dx, uv1_dx), uv_set(bits, 8u, uv_dy, uv1_dy)));
    float metallic = clamp(params.x * mr.z, 0.0, 1.0);
    float roughness = clamp(params.y * mr.y, 0.045, 1.0);
    // The occlusion map's, and the view's ambient occlusion where this
    // pixel is.
    float4 occ_params = float4(u.occlusion);
    float screen_ao = ao_tex.sample(linear_samp, float2(in.position.x * occ_params.y, in.position.y * occ_params.z),
        gradient2d(float2(0.0), float2(0.0))).x;
    float occlusion = (1.0 + params.w * (occlusion_tex.sample(material_samp, uv_set(bits, 32u, in.uv, uv1),
        gradient2d(uv_set(bits, 32u, uv_dx, uv1_dx), uv_set(bits, 32u, uv_dy, uv1_dy))).x - 1.0))
        * (1.0 - occ_params.x * (1.0 - screen_ao));
    float3 emissive = emissive_tex.sample(material_samp, uv_set(bits, 64u, in.uv, uv1),
        gradient2d(uv_set(bits, 64u, uv_dx, uv1_dx), uv_set(bits, 64u, uv_dy, uv1_dy))).xyz
        * float4(material.emissive).xyz;

    float3 f0 = float3(0.04) + (c - float3(0.04)) * metallic;
    float3 albedo = c * (1.0 - metallic);
    float alpha = roughness * roughness;
    // Roughness widened by how fast the normal turns across the pixel, so a
    // highlight never shrinks below what the pixel can hold.
    float variance = 0.15 * (dot(n_dx, n_dx) + dot(n_dy, n_dy));
    float a2 = clamp(alpha * alpha + min(2.0 * variance, 0.2), 0.0, 1.0);
    float nv = max(dot(n, v), 0.0001);

    // The light: diffuse and specular, scaled so a white diffuse surface
    // facing a light of intensity 1 reflects its color.
    float4 light_dir = float4(u.light_dir);
    float3 l = light_dir.xyz;
    float nl = max(dot(n, l), 0.0);
    float3 color = float3(0.0);
    if (nl > 0.0) {
        // The share of the light the shadow lets through: the nearest
        // cascade that holds the point, nudged along its normal and biased
        // as the light grazes it, a 4x4 tent of depth comparisons, handed
        // over to the next cascade across a cascade's outer tenth.
        float shadow = 1.0;
        float4 sp = float4(u.shadow_params);
        if (sp.x > 0.0) {
            float3 ns = n_geom * side;
            float slope = 1.0 - clamp(dot(ns, l), 0.0, 1.0);
            int first = 3;
            for (int ca = 0; ca < 3; ++ca) {
                int kc = 2 - ca;
                float3 qa = cascade_point(float4(u.shadow[kc * 4]), float4(u.shadow[kc * 4 + 1]),
                    float4(u.shadow[kc * 4 + 2]), in.world);
                if (in_cascade(qa)) {
                    first = kc;
                }
            }
            if (first < 3) {
                float visible = 0.0;
                float left = 1.0;
                float e = sp.y;
                for (int cb = 0; cb < 3; ++cb) {
                    float4 extra = float4(u.shadow[cb * 4 + 3]);
                    float3 q = cascade_point(float4(u.shadow[cb * 4]), float4(u.shadow[cb * 4 + 1]),
                        float4(u.shadow[cb * 4 + 2]), in.world + ns * (extra.y * 1.5 * slope));
                    if (cb >= first && left > 0.0 && !in_cascade(q)) {
                        visible = visible + left;
                        left = 0.0;
                    }
                    if (cb >= first && left > 0.0) {
                        float reference = q.z - extra.x * (1.0 + 2.0 * slope);
                        float tu = (q.x * 0.5 + 0.5) / e - 0.5;
                        float tv = (0.5 - q.y * 0.5) / e - 0.5;
                        float fu = tu - floor(tu);
                        float fv = tv - floor(tv);
                        float bu = (floor(tu) + 0.5) * e;
                        float bv = (floor(tv) + 0.5) * e;
                        float sum = 0.0;
                        for (int j = 0; j < 4; ++j) {
                            float sv = clamp(bv + (float(j) - 1.0) * e, 0.5 * e, 1.0 - 0.5 * e);
                            float wv = j == 0 ? 1.0 - fv : (j == 3 ? fv : 1.0);
                            for (int i = 0; i < 4; ++i) {
                                float su = clamp(bu + (float(i) - 1.0) * e, 0.5 * e, 1.0 - 0.5 * e);
                                float wu = i == 0 ? 1.0 - fu : (i == 3 ? fu : 1.0);
                                float4 st = shadow_tex.sample(point_samp, float2((su + float(cb)) / 3.0, sv),
                                    gradient2d(float2(0.0), float2(0.0)));
                                if (reference <= unpack_depth(st)) {
                                    sum = sum + wu * wv;
                                }
                            }
                        }
                        float lit = min(sum / 9.0, 1.0);
                        float handed = clamp((max(abs(q.x), abs(q.y)) - 0.9) * 10.0, 0.0, 1.0);
                        visible = visible + left * (1.0 - handed) * lit;
                        left = left * handed;
                        if (cb == 2) {
                            visible = visible + left;
                        }
                    }
                }
                shadow = 1.0 - sp.x * (1.0 - visible);
            }
        }
        float3 h = normalize(l + v);
        float nh = max(dot(n, h), 0.0);
        float vh = max(dot(v, h), 0.0);
        float k = pow(1.0 - vh, 5.0);
        float3 fresnel = f0 + (float3(1.0) - f0) * k;
        float spec = ggx(nh, a2) * smith(nv, nl, a2);
        float3 brdf = (float3(1.0) - fresnel) * albedo + fresnel * (spec * M_PI_F);
        color = brdf * (float4(u.light_color).xyz * (light_dir.w * nl * shadow));
    }

    // The environment: its irradiance, and its radiance prefiltered for the
    // roughness, both occluded.
    float3 irradiance = float4(u.sh[0]).xyz + float4(u.sh[1]).xyz * n.y + float4(u.sh[2]).xyz * n.z
        + float4(u.sh[3]).xyz * n.x + float4(u.sh[4]).xyz * (n.x * n.y)
        + float4(u.sh[5]).xyz * (n.y * n.z) + float4(u.sh[6]).xyz * (3.0 * n.z * n.z - 1.0)
        + float4(u.sh[7]).xyz * (n.x * n.z) + float4(u.sh[8]).xyz * (n.x * n.x - n.y * n.y);
    irradiance = max(irradiance, float3(0.0));
    float3 r = n * (2.0 * dot(n, v)) - v;
    float lod = roughness * 5.0;
    float2 dir = env_direction(r);
    float k0 = floor(lod);
    float k1 = min(k0 + 1.0, 5.0);
    float3 e0 = env_tex.sample(linear_samp, env_atlas(dir, k0), gradient2d(float2(0.0), float2(0.0))).xyz;
    float3 e1 = env_tex.sample(linear_samp, env_atlas(dir, k1), gradient2d(float2(0.0), float2(0.0))).xyz;
    float3 prefiltered = e0 + (e1 - e0) * (lod - k0);
    float2 ab = env_brdf(roughness, nv);
    float spec_occ = clamp(pow(nv + occlusion, exp2(-16.0 * roughness - 1.0)) - 1.0 + occlusion, 0.0, 1.0);
    float4 ambient = float4(u.ambient);
    color = color + albedo * irradiance * (occlusion * ambient.x)
        + prefiltered * (f0 * ab.x + ab.y) * (spec_occ * ambient.y) + emissive;
    c = tonemap(color * camera.w);
}
float4 fog_color = float4(u.fog);
float fog = 1.0 - exp(-fog_color.w * max(distance - float4(u.ambient).z, 0.0));
c = c + (fog_color.xyz - c) * fog;

// Premultiplied linear light: the target's sRGB format encodes it.
return float4(clamp(c, 0.0, 1.0) * a, a);"
    };
}

const SCENE_FRAGMENT_BODY: &str = concat!(
    scene_fragment_plain!(),
    scene_fragment_head!(),
    scene_fragment_rest!()
);
const SCENE_SKINNED_FRAGMENT_BODY: &str = concat!(
    scene_fragment_plain!(),
    scene_fragment_head!(),
    scene_fragment_face_test!(),
    scene_fragment_rest!()
);
const SCENE_EXTRA_FRAGMENT_BODY: &str = concat!(
    scene_fragment_extra!(),
    scene_fragment_head!(),
    scene_fragment_rest!()
);
const SCENE_EXTRA_SKINNED_FRAGMENT_BODY: &str = concat!(
    scene_fragment_extra!(),
    scene_fragment_head!(),
    scene_fragment_face_test!(),
    scene_fragment_rest!()
);

/// The depth program's fragment, from the base color map's texture
/// coordinate `uv_b` and the vertex color's alpha `vertex_alpha`.
macro_rules! scene_depth_fragment {
    () => {
        "\
float2 uv_dx = dfdx(uv_b);
float2 uv_dy = dfdy(uv_b);
if (in.tint_alpha <= 0.0) {
    discard_fragment();
    return float4(0.0);
}
if (mode.y > 0.5 && mode.y < 1.5) {
    float a = base_tex.sample(material_samp, uv_b, gradient2d(uv_dx, uv_dy)).w
        * float4(material.base_color).w * vertex_alpha * in.tint_alpha;
    if (a < mode.z) {
        discard_fragment();
        return float4(0.0);
    }
}

// 24 bits of depth, a byte to each of red, green and blue.
float d = floor(clamp(in.light_depth, 0.0, 1.0) * 16777215.0);
float hi = floor(d * (1.0 / 65536.0));
float rest = d - hi * 65536.0;
float mid = floor(rest * (1.0 / 256.0));
float lo = rest - mid * 256.0;
return float4(hi, mid, lo, 255.0) / 255.0;"
    };
}

const SCENE_DEPTH_FRAGMENT_BODY: &str = concat!(
    "\
float4 mode = float4(material.mode);
float2 uv_b = in.uv;
float vertex_alpha = 1.0;
",
    scene_depth_fragment!()
);
const SCENE_EXTRA_DEPTH_FRAGMENT_BODY: &str = concat!(
    "\
float4 mode = float4(material.mode);
float2 uv_b = (uint(mode.w) & 4u) != 0u ? in.uv1 : in.uv;
float vertex_alpha = in.vertex_alpha;
",
    scene_depth_fragment!()
);

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
float2 at = float2(u.axis_x) * pixel.x + float2(u.axis_y) * pixel.y + float2(u.shift);
float2 ndc = float2(at.x / vp.x * 2.0 - 1.0,
                    1.0 - at.y / vp.y * 2.0);

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
