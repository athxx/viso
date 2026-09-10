//! The platform system-font seam: the trait the facade implements so this pure
//! crate can request and receive *system* faces and color-glyph rasters without
//! linking any platform font API.
//!
//! `viso-text` never links CoreText / DirectWrite / fontconfig. It describes
//! what it needs — resolve a system face for a query, rasterize a color glyph —
//! as traits; the facade provides the platform implementation. The query
//! carries a role, matching attributes, a language hint, and a sample string of
//! the actual characters that must be covered, so a platform resolver can drive
//! its cascade/fallback and so the negative cache can key on the exact request.

use crate::FontFaceId;
use crate::font_request::{FontRole, FontSlant, FontWeight, FontWidth};

/// A query for a system face: what the resolver needs the platform to supply.
///
/// `sample` is the actual run of characters the face must cover; a platform
/// resolver uses it to drive cascade/fallback, and it is part of the negative
/// cache key so an unsatisfiable request is not repeated.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct SystemFontQuery {
    /// The role to resolve (UI / serif / mono / CJK / emoji).
    pub role: FontRole,
    /// Requested weight.
    pub weight: FontWeight,
    /// Requested width / stretch.
    pub width: FontWidth,
    /// Requested slant.
    pub slant: FontSlant,
    /// BCP-47 language hint, used to disambiguate CJK (empty = none).
    pub lang: String,
    /// The characters that must be covered, driving cascade/fallback.
    pub sample: String,
}

/// A resolved system face returned by the provider: owned sfnt bytes the
/// resolver can register, plus the face index within a collection.
#[derive(Debug)]
pub struct SystemFontResult {
    /// Owned sfnt bytes for the resolved face. A platform that synthesizes a
    /// single face (assembling tables in memory) returns a self-contained sfnt.
    pub bytes: Vec<u8>,
    /// Face index within the returned bytes if they form a collection; 0 for a
    /// single face.
    pub index: u32,
}

/// A rasterized color glyph handed back by the platform: premultiplied RGBA
/// coverage plus its placement.
#[derive(Debug)]
pub struct ColorGlyph {
    /// Bitmap width in pixels.
    pub width: u32,
    /// Bitmap height in pixels.
    pub height: u32,
    /// Premultiplied-alpha RGBA8 pixels, row-major, `width * height * 4` bytes.
    pub rgba: Vec<u8>,
    /// The em size, in pixels, this bitmap was rasterized for.
    pub pixels_per_em: u16,
    /// Placement of the bitmap's top-left relative to the pen origin, in pixels.
    pub origin_px: [f32; 2],
}

/// The platform capability the facade implements: resolve system faces. Color
/// rasterization is a separate capability ([`ColorGlyphRasterizer`]) because a
/// platform may resolve a face without owning its color raster path.
///
/// Implemented outside this crate; `viso-text` links no platform font API.
pub trait SystemFontProvider {
    /// Resolve a system face for the query, if the platform can satisfy it.
    fn resolve_system_face(&self, query: &SystemFontQuery) -> Option<SystemFontResult>;
}

/// The platform's color-glyph raster capability, kept separate from face
/// resolution so a face and its color source can come from different owners.
pub trait ColorGlyphRasterizer {
    /// Rasterize a color glyph for a resolved face at the requested em size, if
    /// that face has a color source for it.
    fn rasterize_color_glyph(
        &self,
        face: FontFaceId,
        glyph: u16,
        pixels_per_em: u16,
    ) -> Option<ColorGlyph>;
}
