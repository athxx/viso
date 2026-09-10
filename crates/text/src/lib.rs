//! `viso-text` — the text and font runtime.
//!
//! This crate is a pure algorithm layer: font resolution and shaping, the
//! world-ready paragraph pipeline (Unicode BiDi, line breaking, and
//! segmentation), and an adaptive glyph representation model. It never links a
//! platform font API — the one platform-facing capability it needs (resolving
//! and rasterizing *system* faces, and rasterizing color glyphs) is expressed
//! as trait seams the facade implements. The actual GPU texture, page, and
//! upload machinery lives in `viso-render`; this crate owns only the identity
//! and residency *metadata* for glyphs.
//!
//! # Module map
//!
//! The module set and the order they are filled in follow the runtime
//! specification. Font pipeline: [`font_request`], [`font_manifest`],
//! [`font_format`], [`resolver`], [`app_fonts`], [`system_fonts`],
//! [`font_provider`], [`fallback`], [`coverage`], [`font_cache`],
//! [`progressive`]. Shaping and world-ready paragraph correctness: [`shaping`],
//! [`segment`], [`bidi`], [`line_break`], [`line_break_tailoring`],
//! [`text_position`], [`caret`], [`hit_test`], [`selection`], [`ime`],
//! [`paragraph`]. Scheduling and glyph representation: [`text_work`],
//! [`glyph_representation`], [`mtsdf`], [`outline_cache`], [`raster_a8`],
//! [`glyph_cache`].
//!
//! # Glyph representation
//!
//! A glyph resolves to one of five representations ([`GlyphImageKind`]) chosen
//! at runtime by Temporal Promotion, not a fixed lane: exact [`MaskA8`] coverage
//! by default, [`ScalableMtsdf`] under sustained transform, retained
//! [`OutlineVector`] at extreme scale, and [`ColorRgba8`] / [`ColorVector`] for
//! color faces. Each representation has its own residency pool, and coverage is
//! always a correct fallback under residency pressure.
//!
//! [`MaskA8`]: GlyphImageKind::MaskA8
//! [`ScalableMtsdf`]: GlyphImageKind::ScalableMtsdf
//! [`OutlineVector`]: GlyphImageKind::OutlineVector
//! [`ColorRgba8`]: GlyphImageKind::ColorRgba8
//! [`ColorVector`]: GlyphImageKind::ColorVector

#![forbid(unsafe_op_in_unsafe_fn)]

// Font pipeline.
pub mod app_fonts;
pub mod coverage;
pub mod fallback;
pub mod font_cache;
pub mod font_format;
pub mod font_manifest;
pub mod font_provider;
pub mod font_request;
pub mod progressive;
pub mod resolver;
pub mod system_fonts;

// Shaping and world-ready paragraph correctness.
pub mod bidi;
pub mod caret;
pub mod hit_test;
pub mod ime;
pub mod line_break;
pub mod line_break_tailoring;
pub mod paragraph;
pub mod segment;
pub mod selection;
pub mod shaping;
pub mod text_position;

// Scheduling and glyph representation.
pub mod glyph_cache;
pub mod glyph_representation;
pub mod mtsdf;
pub mod outline_cache;
pub mod raster_a8;
pub mod text_work;

pub use bidi::{BaseDirection, BidiInfo, BidiLevel, DirectionRun, Paragraph};
pub use font_cache::FontCache;
pub use font_request::{FontRequest, FontRole, FontSlant, FontTarget, FontWeight, FontWidth};
pub use glyph_cache::{Admission, GlyphKey, GlyphResidency};
pub use glyph_representation::GlyphImageKind;
pub use line_break::{BreakOpportunity, LineBreaker};
pub use line_break_tailoring::{LineBreakStrictness, LineBreakTailoring, WordBreak};
pub use raster_a8::{CoverageBitmap, rasterize_coverage};
pub use resolver::{FontResolver, Resolved};
pub use segment::{GraphemeCheckpoint, Segmenter};
pub use shaping::{Direction, ShapedGlyph, ShapedRun, Shaper};
pub use text_position::{CaretAffinity, TextOffset, TextPosition};

/// Stable identity for a resolved font face.
///
/// A face is registered once from owned sfnt bytes; cheap `ttf-parser` /
/// `rustybuzz` faces are reconstructed on demand from that id. The id is a
/// compact integer, never a family string — string lookup does not appear on
/// any steady-state path.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct FontFaceId(pub u32);
