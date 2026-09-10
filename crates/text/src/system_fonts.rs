//! The platform system-font seam: the trait the facade implements so this pure
//! crate can request and receive *system* faces and color-glyph rasters without
//! linking any platform font API.
//!
//! `viso-text` never links CoreText / DirectWrite / fontconfig. It describes
//! what it needs — resolve a system face for a query, rasterize a color glyph —
//! as traits; the facade provides the platform implementation.

use crate::FontFaceId;

/// A query for a system face: what the fallback / resolver needs the platform
/// to supply (family or role plus the attributes to match).
#[derive(Debug, Clone)]
pub struct SystemFontQuery {
    // TODO(TF-P1): requested family/role, weight, width, slant, script hint.
}

/// A resolved system face returned by the provider: owned sfnt bytes the
/// resolver can register.
#[derive(Debug)]
pub struct SystemFontResult {
    // TODO(TF-P1): owned sfnt bytes plus the platform's chosen attributes.
}

/// The platform capability the facade implements: resolve system faces and
/// rasterize color glyphs. Implemented outside this crate.
pub trait SystemFontProvider {
    /// Resolve a system face for the query, if the platform can satisfy it.
    fn resolve_system_face(&self, query: &SystemFontQuery) -> Option<SystemFontResult>;

    /// Rasterize a color glyph for a resolved face, if it has a color source.
    fn rasterize_color_glyph(&self, face: FontFaceId, glyph: u16) -> Option<ColorGlyph>;
}

/// A rasterized color glyph handed back by the platform: premultiplied RGBA
/// coverage plus its placement.
#[derive(Debug)]
pub struct ColorGlyph {
    // TODO(TF-P1): premultiplied RGBA pixels, dimensions, bearing/advance.
}
