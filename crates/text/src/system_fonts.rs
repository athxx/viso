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
    /// The face's PostScript name as the *platform* reports it, when the platform
    /// can supply one. This is the name a color/bitmap or proprietary-outline
    /// (e.g. Apple `hvgl`) face must be re-opened under to rasterize its glyphs
    /// through the platform, and it is authoritative: a synthesized sfnt may carry
    /// only platform-specific name records that a generic `name`-table reader
    /// cannot decode, so the resolver must not fall back to parsing it out of
    /// `bytes`. `None` when the platform does not report one.
    pub postscript_name: Option<String>,
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

/// A [`SystemFontProvider`] that resolves nothing: the explicit "there is no
/// system-font source" seam.
///
/// A WASM/Canvas runtime has no system fonts to draw on — no CSS `system-ui`, no
/// browser local-font enumeration, no OS installed-font access, and the
/// framework ships no implicit bundled default face (spec section 16.1). Such a
/// runtime wires this provider so the App-first / System-second resolution has a
/// System step that categorically answers nothing: a request the application's
/// own manifest does not satisfy resolves to [`crate::resolver::Resolved::Missing`],
/// never to an implicit system or framework face. It is not WASM-specific —
/// any host that deliberately withholds system fonts uses the same seam.
///
/// This is a real closed door, not a stub: resolution stays total and the
/// missing-font policy runs, rather than a silent default face appearing.
#[derive(Debug, Clone, Copy, Default)]
pub struct NoSystemFonts;

impl SystemFontProvider for NoSystemFonts {
    /// Resolve no system face for any query: this seam owns no fonts.
    fn resolve_system_face(&self, _query: &SystemFontQuery) -> Option<SystemFontResult> {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::font_manifest::{AssetRef, FontManifest, ManifestEntry, ScriptCoverageSummary};
    use crate::font_request::FontRequest;
    use crate::resolver::{FontResolver, Resolved};

    #[test]
    fn no_system_fonts_resolves_nothing() {
        // The zero-provider answers no query, whatever its role or attributes.
        let provider = NoSystemFonts;
        assert!(
            provider
                .resolve_system_face(&SystemFontQuery {
                    role: FontRole::Ui,
                    weight: FontWeight::REGULAR,
                    width: FontWidth::NORMAL,
                    slant: FontSlant::Normal,
                    lang: String::new(),
                    sample: "hello".to_owned(),
                })
                .is_none()
        );
    }

    #[test]
    fn wasm_runtime_has_no_implicit_system_or_framework_font() {
        // A WASM/Canvas runtime ships no system fonts and no implicit framework
        // default face (spec section 16.1). Model that runtime as one whose
        // System step is the zero-provider, then ask for every role with an
        // empty manifest — the application declares nothing.
        //
        // Each role must resolve to Missing, so the missing-font policy runs.
        // If any role resolved to a face here, the framework would be smuggling
        // in an implicit system or bundled default face — exactly what section
        // 16.1 forbids.
        let manifest = FontManifest::default();
        let provider = NoSystemFonts;
        let mut resolver = FontResolver::new();

        for role in [
            FontRole::Ui,
            FontRole::Serif,
            FontRole::Mono,
            FontRole::Cjk,
            FontRole::Emoji,
        ] {
            let got = resolver.resolve(&FontRequest::role(role), &manifest, &provider, "");
            assert_eq!(
                got,
                Resolved::Missing,
                "role {role:?} resolved to an implicit face with no system fonts and an empty manifest",
            );
        }

        // An explicitly named family the app does not ship is equally Missing:
        // there is no system source to fall through to.
        assert_eq!(
            resolver.resolve(&FontRequest::family("Helvetica"), &manifest, &provider, ""),
            Resolved::Missing,
        );
    }

    #[test]
    fn packaged_app_font_still_resolves_without_system_fonts() {
        // "No implicit font" is not "no fonts": a WASM project's own packaged
        // family still resolves through the App-first step, even though the
        // System step owns nothing. This keeps section 16.2 honest — packaged
        // fonts remain a project resource under the zero-system-font runtime.
        let manifest = FontManifest::from_declared(
            vec![ManifestEntry {
                family: "Inter".to_owned(),
                weight: FontWeight::REGULAR,
                width: FontWidth::NORMAL,
                slant: FontSlant::Normal,
                face_index: 0,
                color: false,
                coverage: ScriptCoverageSummary::default(),
                asset: AssetRef(1),
            }],
            vec![(FontRole::Ui, "Inter".to_owned())],
        );
        let provider = NoSystemFonts;
        let mut resolver = FontResolver::new();

        // The app-bound UI role resolves to the packaged face.
        assert!(matches!(
            resolver.resolve(&FontRequest::role(FontRole::Ui), &manifest, &provider, ""),
            Resolved::Face(_),
        ));
        // A role the app did not bind still finds no system source.
        assert_eq!(
            resolver.resolve(
                &FontRequest::role(FontRole::Emoji),
                &manifest,
                &provider,
                ""
            ),
            Resolved::Missing,
        );
    }
}
