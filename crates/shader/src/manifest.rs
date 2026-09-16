//! The pipeline manifest: the compile-time-enumerated set of standard render
//! pipelines (architecture section 7 / §7.5).
//!
//! Historically the four built-in shaders were compiled lazily, on the first
//! draw that needed them — `newLibraryWithSource(…)` ran on the hot path the
//! first time a Button painted. §7.1 forbids that: the standard pipelines are a
//! *fixed, known* set, so their backend artifacts (MSL for Metal, a built-in tag
//! for the headless raster) and validated instance reflection must be enumerated
//! ahead of time and prewarmed at device init, never materialized on a draw.
//!
//! This module is that enumeration. [`standard_manifest`] returns one
//! [`PipelineEntry`] per implemented standard pipeline, each carrying:
//!
//! - the [`PipelineFamily`] it belongs to (§7.5) and its packed [`VariantKey`];
//! - the frozen backend MSL, surfaced as `&'static str` — the exact
//!   [`emit_msl`](crate::ir::codegen_msl::emit_msl) output the `msl.rs` accessors
//!   cache once behind a `OnceLock` (byte-equal to the `testdata` oracles), not a
//!   re-derivation;
//! - the [`InstanceSchema`] the shader declares (its reflection), which the
//!   renderer cross-checks against the derived `#[derive(GpuPod)]` layout at
//!   registration (§36.1);
//! - the [`BuiltinShader`] tag the headless raster dispatches on;
//! - the vertex/fragment entry-point names.
//!
//! There is deliberately no `build.rs`: "build-time" here means the compilation
//! phase, and the MSL is already a frozen constant surfaced through the existing
//! `OnceLock` accessors. The manifest is built once, on first call, via a
//! `OnceLock`; the renderer consumes it at device init to create every standard
//! pipeline before the first frame.

use viso_gpu::{BuiltinShader, InstanceSchema};

use crate::msl::{
    ANALYTIC_ELLIPSE_MSL, ANALYTIC_RRECT_MSL, GLYPHRUN_MSL, IMAGE_MSL, MESH_MSL, QUAD_MSL,
    analytic_ellipse_schema, analytic_rrect_schema, glyphrun_schema, image_schema, mesh_schema,
    quad_schema,
};
use std::sync::OnceLock;

/// A standard pipeline family (§7.5).
///
/// A family names a *shape/material class* that maps to one shader and one
/// fixed-function state combination — not a per-draw parameter. Color, radius,
/// opacity, gradient stops, and the like are dynamic instance/uniform data and
/// never spawn a new family or variant (no uber-shader, no permutation
/// explosion).
///
/// The full taxonomy is frozen here as the F2 contract. The four built-ins
/// implemented today populate a subset; the remaining families are declared but
/// have no manifest entry yet — the D drawing layer fills them in as it grows,
/// binding to this same stable enum.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PipelineFamily {
    /// A solid (optionally bordered) axis-aligned rectangle — the Quad built-in.
    SolidRect,
    /// An analytic rounded rectangle (SDF-antialiased). Folded into the Quad
    /// built-in today (its shader is a rounded-rect SDF); a dedicated variant is
    /// a D-layer refinement.
    AnalyticRRect,
    /// An analytic ellipse/circle (SDF-antialiased). No F2 built-in yet.
    AnalyticEllipse,
    /// An analytic line/segment (SDF-antialiased). No F2 built-in yet.
    AnalyticLine,
    /// A textured image quad sampling an atlas/texture — the Image built-in.
    Image,
    /// A gradient fill (linear/radial/…). No F2 built-in yet.
    Gradient,
    /// A filled vector path (per-vertex mesh) — the shared Mesh built-in.
    PathFill,
    /// A stroked vector path (per-vertex mesh). Shares the Mesh built-in today.
    PathStroke,
    /// A coverage/mask composite — the GlyphRun built-in samples an A8 coverage
    /// atlas, the canonical mask-composite case.
    MaskComposite,
}

/// The color-target class a pipeline renders into. Part of [`VariantKey`]
/// because it selects the pipeline's color-attachment format, a
/// pipeline-changing dimension.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ColorTargetClass {
    /// The default 8-bit-per-channel color target (surface and offscreen layers).
    Bgra8,
}

/// A packed key identifying one concrete pipeline within a family (§7.5).
///
/// A variant is created **only** for dimensions that genuinely change the
/// compiled pipeline object or its fixed-function state — the shader family, the
/// color-target class, the sample count, and the depth/stencil class. Dynamic
/// draw parameters (color, radius, opacity, gradient angle, …) are instance or
/// uniform data and never appear here.
///
/// The four fields pack into a single [`u32`] via [`VariantKey::packed`]; the
/// packed integer is what a batch key or pipeline cache uses as a lookup, never
/// a string.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct VariantKey {
    /// The shader family.
    pub family: PipelineFamily,
    /// The color-attachment class.
    pub color_target: ColorTargetClass,
    /// MSAA sample count (1 = no multisampling). A distinct pipeline object.
    pub sample_count: u8,
    /// The depth/stencil class: `true` when a depth attachment is bound.
    pub depth_stencil: bool,
}

impl VariantKey {
    /// The default variant for `family`: the 8-bit color target, no
    /// multisampling, no depth/stencil — the state every standard built-in uses
    /// today.
    pub const fn standard(family: PipelineFamily) -> Self {
        VariantKey {
            family,
            color_target: ColorTargetClass::Bgra8,
            sample_count: 1,
            depth_stencil: false,
        }
    }

    /// Pack the variant into a single [`u32`] cache/batch key. The fields occupy
    /// disjoint bit ranges — family in bits 0..8, color target in bits 8..16,
    /// sample count in bits 16..24, depth/stencil in bit 24 — so the packed
    /// value is a stable, order-free identity for the concrete pipeline.
    pub const fn packed(self) -> u32 {
        let family = self.family as u32; // < 256
        let color = self.color_target as u32; // < 256
        let samples = self.sample_count as u32; // < 256
        let depth = self.depth_stencil as u32; // 0 or 1
        family | (color << 8) | (samples << 16) | (depth << 24)
    }
}

/// One standard pipeline: its family/variant, its frozen backend artifact, the
/// instance reflection it declares, and the entry points.
///
/// Every field is `'static`: `msl` is the frozen `OnceLock`-cached MSL string,
/// `schema` borrows its attributes for `'static`, so an entry is `Copy` and the
/// whole manifest is a small table the renderer indexes at device init.
#[derive(Debug, Clone, Copy)]
pub struct PipelineEntry {
    /// The family this pipeline implements.
    pub family: PipelineFamily,
    /// The packed variant key.
    pub variant: VariantKey,
    /// The built-in tag the headless raster dispatches on (Metal ignores it).
    pub builtin: BuiltinShader,
    /// The frozen backend MSL (Metal). Byte-equal to the `testdata` oracle;
    /// compiled once at device-init prewarm, never on a draw.
    pub msl: &'static str,
    /// The instance layout the shader declares (its reflection), cross-checked
    /// against the derived `#[derive(GpuPod)]` layout at registration (§36.1).
    pub schema: InstanceSchema,
    /// Vertex entry-point name.
    pub vertex_entry: &'static str,
    /// Fragment entry-point name.
    pub fragment_entry: &'static str,
}

/// The enumerated set of standard pipelines (§7.1/§7.5).
///
/// A small owned table of [`PipelineEntry`]s. Built once via [`standard_manifest`]
/// and consumed by the renderer at device init to create every standard pipeline
/// before the first frame — the prewarm that keeps `newLibraryWithSource` off the
/// draw path.
#[derive(Debug, Clone)]
pub struct PipelineManifest {
    entries: Vec<PipelineEntry>,
}

impl PipelineManifest {
    /// The standard pipeline entries, in a stable order.
    pub fn entries(&self) -> &[PipelineEntry] {
        &self.entries
    }

    /// The entry for `family`'s standard variant, or `None` if that family has
    /// no built-in pipeline yet.
    pub fn entry(&self, family: PipelineFamily) -> Option<&PipelineEntry> {
        self.entries.iter().find(|e| e.family == family)
    }
}

/// The process-wide standard pipeline manifest (§7.1).
///
/// Materialized once on first call (a cold, device-init path) and cached in a
/// `OnceLock`; every later call returns the same `&'static` table with no
/// allocation. This is the single source the renderer prewarms from — the
/// Impeller-like "compile the fixed set ahead of time" contract.
pub fn standard_manifest() -> &'static PipelineManifest {
    static MANIFEST: OnceLock<PipelineManifest> = OnceLock::new();
    MANIFEST.get_or_init(|| PipelineManifest {
        entries: vec![
            PipelineEntry {
                family: PipelineFamily::SolidRect,
                variant: VariantKey::standard(PipelineFamily::SolidRect),
                builtin: BuiltinShader::Quad,
                msl: QUAD_MSL(),
                schema: quad_schema(),
                vertex_entry: "vertex_main",
                fragment_entry: "fragment_main",
            },
            PipelineEntry {
                family: PipelineFamily::Image,
                variant: VariantKey::standard(PipelineFamily::Image),
                builtin: BuiltinShader::Image,
                msl: IMAGE_MSL(),
                schema: image_schema(),
                vertex_entry: "vertex_main",
                fragment_entry: "fragment_main",
            },
            PipelineEntry {
                family: PipelineFamily::MaskComposite,
                variant: VariantKey::standard(PipelineFamily::MaskComposite),
                builtin: BuiltinShader::GlyphRun,
                msl: GLYPHRUN_MSL(),
                schema: glyphrun_schema(),
                vertex_entry: "vertex_main",
                fragment_entry: "fragment_main",
            },
            PipelineEntry {
                family: PipelineFamily::PathFill,
                variant: VariantKey::standard(PipelineFamily::PathFill),
                builtin: BuiltinShader::Path,
                msl: MESH_MSL(),
                schema: mesh_schema(),
                vertex_entry: "vertex_main",
                fragment_entry: "fragment_main",
            },
            PipelineEntry {
                family: PipelineFamily::AnalyticRRect,
                variant: VariantKey::standard(PipelineFamily::AnalyticRRect),
                builtin: BuiltinShader::AnalyticRRect,
                msl: ANALYTIC_RRECT_MSL(),
                schema: analytic_rrect_schema(),
                vertex_entry: "vertex_main",
                fragment_entry: "fragment_main",
            },
            PipelineEntry {
                family: PipelineFamily::AnalyticEllipse,
                variant: VariantKey::standard(PipelineFamily::AnalyticEllipse),
                builtin: BuiltinShader::AnalyticEllipse,
                msl: ANALYTIC_ELLIPSE_MSL(),
                schema: analytic_ellipse_schema(),
                vertex_entry: "vertex_main",
                fragment_entry: "fragment_main",
            },
        ],
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ir::testdata::{
        ANALYTIC_ELLIPSE_MSL_ORIGINAL, ANALYTIC_RRECT_MSL_ORIGINAL, GLYPHRUN_MSL_ORIGINAL,
        IMAGE_MSL_ORIGINAL, MESH_MSL_ORIGINAL, QUAD_MSL_ORIGINAL,
    };

    #[test]
    fn manifest_enumerates_the_standard_builtins() {
        let m = standard_manifest();
        assert_eq!(m.entries().len(), 6);
        assert!(m.entry(PipelineFamily::SolidRect).is_some());
        assert!(m.entry(PipelineFamily::Image).is_some());
        assert!(m.entry(PipelineFamily::MaskComposite).is_some());
        assert!(m.entry(PipelineFamily::PathFill).is_some());
        assert!(m.entry(PipelineFamily::AnalyticRRect).is_some());
        assert!(m.entry(PipelineFamily::AnalyticEllipse).is_some());
    }

    #[test]
    fn families_without_a_builtin_have_no_entry() {
        let m = standard_manifest();
        for family in [
            PipelineFamily::AnalyticLine,
            PipelineFamily::Gradient,
            PipelineFamily::PathStroke,
        ] {
            assert!(m.entry(family).is_none(), "{family:?} has no F2 built-in");
        }
    }

    #[test]
    fn manifest_msl_is_the_frozen_oracle() {
        // The manifest surfaces the exact frozen MSL — byte-equal to the
        // testdata oracles — not a fresh derivation.
        let m = standard_manifest();
        assert_eq!(
            m.entry(PipelineFamily::SolidRect).unwrap().msl,
            QUAD_MSL_ORIGINAL
        );
        assert_eq!(
            m.entry(PipelineFamily::Image).unwrap().msl,
            IMAGE_MSL_ORIGINAL
        );
        assert_eq!(
            m.entry(PipelineFamily::MaskComposite).unwrap().msl,
            GLYPHRUN_MSL_ORIGINAL
        );
        assert_eq!(
            m.entry(PipelineFamily::PathFill).unwrap().msl,
            MESH_MSL_ORIGINAL
        );
        assert_eq!(
            m.entry(PipelineFamily::AnalyticRRect).unwrap().msl,
            ANALYTIC_RRECT_MSL_ORIGINAL
        );
        assert_eq!(
            m.entry(PipelineFamily::AnalyticEllipse).unwrap().msl,
            ANALYTIC_ELLIPSE_MSL_ORIGINAL
        );
    }

    #[test]
    fn variant_key_packs_disjoint_fields() {
        // Distinct families pack to distinct keys; the family occupies the low
        // byte and the default single-sample no-depth state sets sample_count=1
        // in bits 16..24.
        let rect = VariantKey::standard(PipelineFamily::SolidRect).packed();
        let image = VariantKey::standard(PipelineFamily::Image).packed();
        assert_ne!(rect, image);
        assert_eq!(rect & 0xff, PipelineFamily::SolidRect as u32);
        assert_eq!((rect >> 16) & 0xff, 1);

        // Changing any pipeline-changing dimension changes the key.
        let msaa = VariantKey {
            sample_count: 4,
            ..VariantKey::standard(PipelineFamily::SolidRect)
        };
        assert_ne!(msaa.packed(), rect);
        assert_eq!(msaa.packed() >> 16 & 0xff, 4);

        let depth = VariantKey {
            depth_stencil: true,
            ..VariantKey::standard(PipelineFamily::SolidRect)
        };
        assert_ne!(depth.packed(), rect);
        assert_eq!(depth.packed() >> 24 & 0x1, 1);
    }
}
