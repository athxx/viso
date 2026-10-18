//! System fonts: the platform bindings behind [`viso_text::SystemFontProvider`]
//! and [`viso_text::ColorGlyphRasterizer`].
//!
//! viso-text stays a pure algorithm layer, so each OS query lives here in the
//! facade, one module per platform font API:
//!
//! | target                          | resolver                       | module         |
//! |---------------------------------|--------------------------------|----------------|
//! | macOS / iOS                     | CoreText cascade               | `coretext`     |
//! | Windows                         | DirectWrite system collection  | `directwrite`  |
//! | Linux / BSD (not Android)       | fontconfig, loaded at runtime  | `fontconfig`   |
//! | Android                         | `AFontMatcher`, else `fonts.xml` | `android`    |
//! | everything else (WASM)          | none — the application's fonts | —              |
//! | a `headless` or embedded build  | none — the application's fonts | —              |
//!
//! Every resolver answers the same question — a face for a role, style,
//! language and sample run — with the face's own file bytes and collection
//! index, so shaping, coverage and the portable color raster
//! ([`viso_text::raster_color`]) read the real tables. Only Apple targets
//! rasterize glyphs through the platform ([`PlatformColorRaster`]); elsewhere color glyphs
//! are painted from the face's `COLR` / `CBDT` / `sbix` tables and the platform
//! raster declines.
//!
//! A headless build reads no system font on any target, so what it draws
//! depends on the project alone; nor does an embedded profile unless built
//! with `system-fonts` (both are `cfg(viso_no_system_fonts)`).

#[cfg(all(target_os = "android", not(viso_no_system_fonts)))]
mod android;
#[cfg(any(target_os = "android", test))]
mod android_config;
#[cfg(all(any(target_os = "macos", target_os = "ios"), not(viso_no_system_fonts)))]
mod coretext;
#[cfg(all(target_os = "windows", not(viso_no_system_fonts)))]
mod directwrite;
#[cfg(all(
    unix,
    not(any(target_os = "macos", target_os = "ios")),
    not(viso_no_system_fonts)
))]
mod dylib;
#[cfg(all(
    unix,
    not(any(target_os = "macos", target_os = "ios", target_os = "android")),
    not(viso_no_system_fonts)
))]
mod fontconfig;
#[cfg(any(
    all(
        any(
            target_os = "windows",
            all(unix, not(any(target_os = "macos", target_os = "ios")))
        ),
        not(viso_no_system_fonts)
    ),
    test
))]
mod sample;

#[cfg(all(any(target_os = "macos", target_os = "ios"), not(viso_no_system_fonts)))]
pub use coretext::{
    CoreTextColorRaster as PlatformColorRaster, CoreTextProvider as PlatformFontProvider,
    LiveFontRegistry,
};

#[cfg(any(not(any(target_os = "macos", target_os = "ios")), viso_no_system_fonts))]
pub use declining::{LiveFontRegistry, NoColorRaster as PlatformColorRaster};

#[cfg(all(target_os = "windows", not(viso_no_system_fonts)))]
pub use directwrite::DirectWriteProvider as PlatformFontProvider;

#[cfg(all(
    unix,
    not(any(target_os = "macos", target_os = "ios", target_os = "android")),
    not(viso_no_system_fonts)
))]
pub use fontconfig::FontconfigProvider as PlatformFontProvider;

#[cfg(all(target_os = "android", not(viso_no_system_fonts)))]
pub use android::AndroidFontProvider as PlatformFontProvider;

#[cfg(any(not(any(target_os = "windows", unix)), viso_no_system_fonts))]
pub use no_system::NoSystemFonts as PlatformFontProvider;

/// The shared pieces for targets whose platform raster has nothing to add:
/// no live font handles to share, and a color raster that always declines so
/// color glyphs come from the portable raster alone.
#[cfg(any(not(any(target_os = "macos", target_os = "ios")), viso_no_system_fonts))]
mod declining {
    /// A zero-sized, cheap-to-clone stand-in for the Apple registry of live
    /// CoreText handles, so the shaper is built the same way on every target.
    #[derive(Clone, Default)]
    pub struct LiveFontRegistry;

    impl LiveFontRegistry {
        pub fn new() -> Self {
            Self
        }
    }

    /// A color raster that declines every glyph.
    pub struct NoColorRaster;

    impl NoColorRaster {
        pub fn new(_live: LiveFontRegistry) -> Self {
            Self
        }

        /// Nothing to bind: the portable raster reads the face bytes directly.
        pub fn register_face(
            &self,
            _face: viso_text::FontFaceId,
            _ps_name: &str,
            _expected_glyph_count: u16,
        ) {
        }

        pub fn forget_face(&self, _face: viso_text::FontFaceId) {}

        /// Grayscale recovery of outlines the parser cannot read is a CoreText
        /// path (`hvgl`); nothing else needs it.
        pub fn rasterize_coverage_glyph(
            &self,
            _face: viso_text::FontFaceId,
            _glyph: u16,
            _pixels_per_em: u16,
        ) -> Option<viso_text::CoverageBitmap> {
            None
        }
    }

    impl viso_text::ColorGlyphRasterizer for NoColorRaster {
        fn rasterize_color_glyph(
            &self,
            _face: viso_text::FontFaceId,
            _glyph: u16,
            _pixels_per_em: u16,
        ) -> Option<viso_text::ColorGlyph> {
            None
        }
    }
}

/// A target with no system-font source, or a headless build.
#[cfg(any(not(any(target_os = "windows", unix)), viso_no_system_fonts))]
mod no_system {
    use super::LiveFontRegistry;

    /// Every query declines, so text resolves through the application's own
    /// fonts only.
    pub struct NoSystemFonts;

    impl NoSystemFonts {
        pub fn new(_live: LiveFontRegistry) -> Self {
            Self
        }
    }

    impl viso_text::SystemFontProvider for NoSystemFonts {
        fn resolve_system_face(
            &self,
            _query: &viso_text::SystemFontQuery,
        ) -> Option<viso_text::SystemFontResult> {
            None
        }
    }
}
