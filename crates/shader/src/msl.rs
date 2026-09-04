//! The per-primitive MSL/schema surface `viso-render` consumes, now *derived*
//! from the typed shader [`ir`](crate::ir) rather than hand-written.
//!
//! Each built-in used to carry two hand-maintained copies of one field contract —
//! a `packed_float*` MSL `struct` and a parallel [`InstanceSchema`] — kept in
//! lockstep only by name-order test assertions (the section-56 "implicit shader
//! instance field-order ABI"). Both now project from one [`ShaderIr`] value: the
//! `*_MSL()` accessors return [`emit_msl`](crate::ir::codegen_msl::emit_msl) of
//! the primitive's IR and the `*_schema()` fns return
//! [`emit_schema_attrs`](crate::ir::codegen_msl::emit_schema_attrs) of the *same*
//! IR, so the MSL struct and the validated schema cannot drift.
//!
//! The MSL and schema are the shader half of the three-way instance contract (the
//! other two halves are the `#[derive(GpuInstance)]` layout and the headless
//! rasterizer's field reader); `create_pipeline` validates the derived layout
//! against the schema at registration.
//!
//! ## Derivation is a one-time cold cost, exposed as `&'static`
//!
//! `PipelineDesc::shader_source` is `&'static str` and `InstanceSchema` borrows
//! its attributes for `'static`, but the IR-derived MSL string and schema
//! attribute vector are computed at run time. Each accessor caches its result in a
//! function-local `static OnceLock`, so the first call (pipeline registration, a
//! cold path) materializes the value and every later call returns the same
//! `&'static` view with zero allocation — no steady-state cost.
//!
//! Reserved-word caveat: `half` is an MSL type (16-bit float) and never appears as
//! an identifier — the IR type printer only emits `float`/`uint` (see
//! `viso-msl-reserved-half`); the headless backend does not compile MSL, so a
//! change to any body fragment must still be verified on a real Metal device.

use std::sync::OnceLock;

use viso_gpu::{InstanceSchema, SchemaAttr};

use crate::ir::codegen_msl::{emit_msl, emit_schema_attrs, schema_from_attrs};
use crate::ir::module::{ShaderIr, glyphrun_ir, image_ir, mesh_ir, quad_ir};

/// The built-in primitive shaders (architecture section 15.3). One entry per
/// [`Primitive`] kind in `viso-render`; each maps to an IR-derived MSL program and
/// an instance schema.
///
/// Quad, Image, GlyphRun, Path, and Mesh are implemented; [`PrimitiveKind::Layer`]
/// returns `None` from [`shader_source`]/[`instance_schema`] (it is a clip
/// container, not a shaded primitive — see the renderer's scissor handling).
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

/// The MSL source for `kind`, or `None` if that primitive has no shader.
pub fn shader_source(kind: PrimitiveKind) -> Option<&'static str> {
    match kind {
        PrimitiveKind::Quad => Some(QUAD_MSL()),
        PrimitiveKind::Image => Some(IMAGE_MSL()),
        PrimitiveKind::GlyphRun => Some(GLYPHRUN_MSL()),
        // Path and Mesh share the general per-vertex mesh pipeline.
        PrimitiveKind::Path | PrimitiveKind::Mesh => Some(MESH_MSL()),
        _ => None,
    }
}

/// The instance schema `kind`'s shader declares, or `None` if that primitive is
/// not implemented. This is what the pipeline validates the derived `GpuInstance`
/// layout against at registration.
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

/// Cache a primitive's IR-derived schema attributes to `'static` and return the
/// [`InstanceSchema`] borrowing them. The `OnceLock` is materialized once on the
/// cold registration path (running the IR → `SchemaAttr` projection); every later
/// call returns the same `&'static` slice.
fn cached_schema(cell: &'static OnceLock<Vec<SchemaAttr>>, ir: &ShaderIr) -> InstanceSchema {
    let attrs = cell.get_or_init(|| emit_schema_attrs(ir));
    schema_from_attrs(attrs)
}

/// The instance schema the Quad shader declares — projected from [`quad_ir`], the
/// single source of truth for the Quad field contract. `viso-render`'s
/// `QuadInstance` derive is validated against it, and the fields map 1:1 onto the
/// same IR's `InstanceIn` in [`QUAD_MSL`].
pub fn quad_schema() -> InstanceSchema {
    static CELL: OnceLock<Vec<SchemaAttr>> = OnceLock::new();
    cached_schema(&CELL, &quad_ir())
}

/// The instance schema the Image shader declares — projected from [`image_ir`].
///
/// Unlike makepad's `DrawImage` (which derives the UV from the unit-quad corner
/// and has no atlas sub-rect), Viso carries an explicit per-instance UV sub-rect
/// (`uv_pos`/`uv_size`) so the same path serves atlas/glyph sub-regions later.
pub fn image_schema() -> InstanceSchema {
    static CELL: OnceLock<Vec<SchemaAttr>> = OnceLock::new();
    cached_schema(&CELL, &image_ir())
}

/// The instance schema the GlyphRun shader declares — projected from
/// [`glyphrun_ir`].
///
/// Structurally this is the Image schema plus a per-instance `px_range`: a glyph
/// is a textured rect sampling the single-channel R8 SDF atlas, and `px_range`
/// tells the fragment shader how many stored-units span one screen pixel so it can
/// turn the sampled signed distance back into antialiased coverage.
pub fn glyphrun_schema() -> InstanceSchema {
    static CELL: OnceLock<Vec<SchemaAttr>> = OnceLock::new();
    cached_schema(&CELL, &glyphrun_ir())
}

/// The per-vertex schema the general mesh shader declares — projected from
/// [`mesh_ir`], shared by Path and Mesh.
///
/// Unlike the Quad/Image schemas — which describe *per-instance* data — this
/// describes *per-vertex* data: the mesh pipeline draws a real indexed triangle
/// list from a vertex buffer, not a `vertex_id`-generated quad. The same 4-byte-
/// aligned attribute descriptors serve both (a vertex layout is structurally
/// identical to an instance layout).
pub fn mesh_schema() -> InstanceSchema {
    static CELL: OnceLock<Vec<SchemaAttr>> = OnceLock::new();
    cached_schema(&CELL, &mesh_ir())
}

/// Cache a primitive's IR-derived MSL to `'static` and return it. Materialized
/// once on the cold registration path; every later call returns the same string.
fn cached_msl(cell: &'static OnceLock<String>, ir: impl FnOnce() -> String) -> &'static str {
    cell.get_or_init(ir).as_str()
}

/// Inline MSL for the Quad built-in (Metal backend), derived from [`quad_ir`].
///
/// The headless raster backend ignores this and dispatches on
/// `BuiltinShader::Quad`; only the real Metal backend compiles it, so MSL
/// syntax/reserved-word errors surface only when a device pipeline is created (run
/// `viso-example-hello-world`). See `viso-msl-reserved-half`.
///
/// Contract (guaranteed by the shared IR, not by hand): per-instance data is a raw
/// buffer at index 1 (`InstanceIn`, `packed_float*` field types/order matching the
/// `#[repr(C)]` struct); the viewport `[width, height]` is an inline uniform at
/// index 0; six `vertex_id`s form two triangles (plus 1px AA pad); colors are
/// **straight** and the fragment premultiplies (blend `src One`, `dst
/// OneMinusSourceAlpha`); a rounded-rect SDF with linear-coverage AA and
/// border-over-fill reproduces the headless math.
#[allow(non_snake_case)]
pub fn QUAD_MSL() -> &'static str {
    static CELL: OnceLock<String> = OnceLock::new();
    cached_msl(&CELL, || emit_msl(&quad_ir()))
}

/// Inline MSL for the Image built-in (Metal backend), derived from [`image_ir`].
///
/// Like [`QUAD_MSL`], the headless backend ignores this; only the real Metal
/// backend compiles it. See `viso-msl-reserved-half`.
///
/// Contract (guaranteed by the shared IR): per-instance data at buffer index 1;
/// viewport uniform at index 0; the bound texture at `[[texture(0)]]`, its sampler
/// at `[[sampler(0)]]`; the UV interpolates across the sub-rect `uv_pos + corner *
/// uv_size`; `color` is a **straight** tint and the sampled texel is premultiplied
/// linear, so the fragment outputs premultiplied.
#[allow(non_snake_case)]
pub fn IMAGE_MSL() -> &'static str {
    static CELL: OnceLock<String> = OnceLock::new();
    cached_msl(&CELL, || emit_msl(&image_ir()))
}

/// Inline MSL for the GlyphRun built-in (Metal backend), derived from
/// [`glyphrun_ir`].
///
/// Like [`QUAD_MSL`]/[`IMAGE_MSL`], only the real Metal backend compiles it. See
/// `viso-msl-reserved-half`.
///
/// Contract (guaranteed by the shared IR): per-instance data at buffer index 1;
/// viewport uniform at index 0; the atlas is a single-channel R8 texture at
/// `[[texture(0)]]` with a linear-clamp sampler at `[[sampler(0)]]`. The atlas
/// stores an ESDT signed distance with the glyph edge (`d = 0`) at `stored = 0.75`
/// (`1 - cutoff`, cutoff fixed at 0.25 in `viso-text`'s rasterizer); coverage is
/// recovered as `clamp((sd - 0.75) * px_range + 0.5, 0, 1)`, matching the headless
/// `fill_glyph` decode. `color` is **straight** and the fragment outputs
/// premultiplied.
#[allow(non_snake_case)]
pub fn GLYPHRUN_MSL() -> &'static str {
    static CELL: OnceLock<String> = OnceLock::new();
    cached_msl(&CELL, || emit_msl(&glyphrun_ir()))
}

/// Inline MSL for the general mesh built-in (Path/Mesh, Metal backend), derived
/// from [`mesh_ir`].
///
/// Unlike [`QUAD_MSL`]/[`IMAGE_MSL`], this reads a **real per-vertex buffer** at
/// index 0 indexed by `vertex_id` (the CPU tessellator emits absolute indices via
/// `drawIndexedPrimitives`), rather than synthesizing a unit quad; there is no
/// per-instance buffer, so the viewport uniform moves to **buffer index 1**. Only
/// the real Metal backend compiles this. See `viso-msl-reserved-half`.
///
/// Contract (guaranteed by the shared IR): `pos` is in physical pixels (top-left
/// origin), mapped to NDC with a Y flip; `color` is **straight** linear RGBA;
/// `edge` is a `[0, 1]` coverage weight (1 in the interior, ramping to 0 at
/// antialiased fringe vertices); the fragment multiplies alpha by the interpolated
/// `edge` and premultiplies.
#[allow(non_snake_case)]
pub fn MESH_MSL() -> &'static str {
    static CELL: OnceLock<String> = OnceLock::new();
    cached_msl(&CELL, || emit_msl(&mesh_ir()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ir::module::{glyphrun_ir, image_ir, mesh_ir, quad_ir};

    /// The three legs of a built-in's field contract — the emitted MSL attribute
    /// struct, the derived schema, and the IR attribute list — must agree field for
    /// field, in order. Since all three now project from one `ShaderIr`, this can
    /// only fail if codegen or the schema projection regresses.
    fn assert_three_legs_agree(msl: &str, schema: &InstanceSchema, ir_names: &[&str]) {
        // Schema attribute order == IR attribute field order.
        let schema_names: Vec<_> = schema.attributes.iter().map(|a| a.name).collect();
        assert_eq!(schema_names, ir_names, "schema order == IR attribute order");

        // MSL attribute struct field order == the same list. Parse the struct body
        // between the first `struct InstanceIn`/`struct VertexIn` and its `};`.
        let struct_kw = if msl.contains("struct InstanceIn {") {
            "struct InstanceIn {"
        } else {
            "struct VertexIn {"
        };
        let start = msl.find(struct_kw).expect("attribute struct present") + struct_kw.len();
        let end = start + msl[start..].find("};").expect("attribute struct closes");
        let msl_names: Vec<&str> = msl[start..end]
            .lines()
            .filter_map(|l| l.trim().strip_suffix(';'))
            // Each field is `<type> <name>`; the name is the last whitespace token.
            .map(|decl| decl.rsplit(char::is_whitespace).next().unwrap())
            .collect();
        assert_eq!(
            msl_names, ir_names,
            "MSL struct field order == IR attribute order"
        );
    }

    #[test]
    fn quad_has_source_and_schema() {
        assert!(shader_source(PrimitiveKind::Quad).is_some());
        assert!(instance_schema(PrimitiveKind::Quad).is_some());
        let ir_names: Vec<&str> = quad_ir().attributes.iter().map(|f| f.name).collect();
        assert_eq!(
            ir_names,
            [
                "rect_pos",
                "rect_size",
                "color",
                "radius",
                "border_width",
                "border_color"
            ]
        );
        assert_three_legs_agree(QUAD_MSL(), &quad_schema(), &ir_names);
    }

    #[test]
    fn image_has_source_and_schema() {
        assert!(shader_source(PrimitiveKind::Image).is_some());
        assert!(instance_schema(PrimitiveKind::Image).is_some());
        let ir_names: Vec<&str> = image_ir().attributes.iter().map(|f| f.name).collect();
        assert_eq!(
            ir_names,
            ["rect_pos", "rect_size", "uv_pos", "uv_size", "color"]
        );
        assert_three_legs_agree(IMAGE_MSL(), &image_schema(), &ir_names);
    }

    #[test]
    fn mesh_has_source_and_schema() {
        // Path and Mesh share the general per-vertex mesh pipeline.
        for kind in [PrimitiveKind::Path, PrimitiveKind::Mesh] {
            assert!(shader_source(kind).is_some());
            assert!(instance_schema(kind).is_some());
        }
        let ir_names: Vec<&str> = mesh_ir().attributes.iter().map(|f| f.name).collect();
        assert_eq!(ir_names, ["pos", "color", "edge"]);
        assert_three_legs_agree(MESH_MSL(), &mesh_schema(), &ir_names);
    }

    #[test]
    fn glyphrun_has_source_and_schema() {
        assert!(shader_source(PrimitiveKind::GlyphRun).is_some());
        assert!(instance_schema(PrimitiveKind::GlyphRun).is_some());
        let ir_names: Vec<&str> = glyphrun_ir().attributes.iter().map(|f| f.name).collect();
        assert_eq!(
            ir_names,
            [
                "rect_pos",
                "rect_size",
                "uv_pos",
                "uv_size",
                "color",
                "px_range"
            ]
        );
        assert_three_legs_agree(GLYPHRUN_MSL(), &glyphrun_schema(), &ir_names);
    }

    #[test]
    fn layer_has_no_shader() {
        // Layer is a rectangular scissor clip, not a drawn primitive, so it has
        // neither an MSL source nor an instance schema.
        assert!(shader_source(PrimitiveKind::Layer).is_none());
        assert!(instance_schema(PrimitiveKind::Layer).is_none());
    }
}
