//! CoreText system-font provider: the macOS platform binding behind
//! [`viso_text::SystemFontProvider`].
//!
//! viso-text stays a pure algorithm layer, so the actual OS query lives here in
//! the facade. Given a role and a sample string, we ask CoreText for the base UI
//! font, coax it to cascade to a face covering the sample (CJK / emoji), reject
//! the LastResort tofu font, and reassemble the face's sfnt tables into a byte
//! blob that ttf-parser / rustybuzz can parse.
//!
//! On non-macOS targets this module compiles to a stub whose provider always
//! returns `None`; the bundled fallback faces cover those platforms until their
//! own providers land (the trait seam is already in place).
//!
//! ## sfnt reassembly (the interesting part)
//!
//! CoreText hands back a live font object, not a file. `sfnt_bytes_from_ctfont`
//! walks its table directory (`CTFontCopyAvailableTables`), copies each table
//! (`CTFontCopyTable`), and rebuilds a valid sfnt container. Two constraints
//! keep this path bounded:
//!
//!   - **Skip color-bitmap tables** (`sbix`, `CBDT`, `CBLC`, `COLR`, `CPAL`).
//!     AppleColorEmoji's `sbix` table is ~179 MB, and even copied ttf-parser
//!     could not decode its private image format; instead the emoji face is
//!     marked [`is_color_emoji`](viso_text::FontFace::is_color_emoji) and its
//!     color glyphs are rasterized on demand by [`CoreTextColorRaster`] — handing
//!     the stripped glyph back to CoreText, keyed on the face's PostScript name.
//!     So copying the strike would be pure waste.
//!   - **Drop variable-font tables** (`gvar`, `fvar`, ...) when a `glyf` table is
//!     present. ttf-parser cannot resolve Apple's `gvar` deltas, so leaving them
//!     in yields blank glyphs; we consume only the default instance.
//!
//! Per-table checksums and `head.checkSumAdjustment` are left zero: ttf-parser
//! does not verify them, and computing them over Apple's giant CJK super-fonts
//! costs tens of milliseconds for no benefit.

#[cfg(target_os = "macos")]
pub use imp::CoreTextProvider;

/// A no-op provider for platforms without a native binding yet. Always resolves
/// `None`, so the fallback chain relies on bundled faces there.
#[cfg(not(target_os = "macos"))]
pub struct CoreTextProvider;

#[cfg(not(target_os = "macos"))]
impl CoreTextProvider {
    /// Takes the shared registry for signature parity with the macOS provider;
    /// it owns no fonts to record.
    pub fn new(_live: LiveFontRegistry) -> Self {
        Self
    }
}

#[cfg(not(target_os = "macos"))]
impl viso_text::SystemFontProvider for CoreTextProvider {
    fn resolve_system_face(
        &self,
        _query: &viso_text::SystemFontQuery,
    ) -> Option<viso_text::SystemFontResult> {
        None
    }
}

#[cfg(target_os = "macos")]
pub use color::CoreTextColorRaster;
pub use live_fonts::LiveFontRegistry;

/// A no-op registry stub for platforms without a live-`CTFont` source: the
/// provider records nothing and the raster reads nothing, so the shared
/// [`TextShaper`](crate::text_content) wiring compiles unchanged off macOS.
#[cfg(not(target_os = "macos"))]
mod live_fonts {
    /// A zero-sized, cheap-to-clone stub mirroring the macOS registry's shape so
    /// the same shaper construction works on every platform.
    #[derive(Clone, Default)]
    pub struct LiveFontRegistry;

    impl LiveFontRegistry {
        pub fn new() -> Self {
            Self
        }
    }
}

/// The shared registry of live CoreText font handles that the system-font
/// provider resolves and the color/coverage raster reuses.
///
/// A face reaches `viso-text` as reassembled sfnt bytes plus a PostScript name,
/// but its rasterizable identity is the *live* `CTFont` CoreText already
/// resolved during the cascade. Re-deriving that handle from the name is
/// unreliable: Apple's system faces report private `.`-prefixed PostScript names
/// (`.PingFangUITextSC-Regular`) that `CTFontCreateWithName` refuses, silently
/// substituting a Latin fallback. So the provider stashes the exact handle it
/// resolved here, keyed by that same name, and the raster copies it to each pixel
/// size with no name round-trip — correct by construction, and one retained
/// handle per distinct face rather than one per size.
#[cfg(target_os = "macos")]
mod live_fonts {
    use std::cell::RefCell;
    use std::collections::HashMap;
    use std::rc::Rc;

    use objc2_core_foundation::CFRetained;
    use objc2_core_text::CTFont;

    /// A cheap-to-clone handle onto the shared name → live-`CTFont` map. Cloning
    /// shares the same map (the provider writes, the raster reads); the whole
    /// thing is main-thread only, matching the rest of the text runtime.
    #[derive(Clone, Default)]
    pub struct LiveFontRegistry {
        fonts: Rc<RefCell<HashMap<String, CFRetained<CTFont>>>>,
    }

    impl LiveFontRegistry {
        pub fn new() -> Self {
            Self::default()
        }

        /// Record the live handle CoreText resolved for `ps_name`. Idempotent:
        /// the first covering face wins, later identical resolutions no-op.
        pub fn insert(&self, ps_name: String, font: CFRetained<CTFont>) {
            self.fonts.borrow_mut().entry(ps_name).or_insert(font);
        }

        /// The live handle previously resolved for `ps_name`, if any.
        pub fn get(&self, ps_name: &str) -> Option<CFRetained<CTFont>> {
            self.fonts.borrow().get(ps_name).cloned()
        }
    }
}

/// A no-op color rasterizer for platforms without a native binding. Always
/// declines, so a color-emoji face yields no color glyph and the shaper falls
/// through to the outline path (no color output, no panic).
#[cfg(not(target_os = "macos"))]
pub struct CoreTextColorRaster;

#[cfg(not(target_os = "macos"))]
impl CoreTextColorRaster {
    /// Takes the shared registry for signature parity with the macOS raster; it
    /// reads no handles.
    pub fn new(_live: LiveFontRegistry) -> Self {
        Self
    }

    /// Bind a resolved emoji face's identity to the CoreText re-open name and
    /// glyph count the raster needs. A no-op on platforms without a binding.
    pub fn register_face(
        &self,
        _face: viso_text::FontFaceId,
        _ps_name: &str,
        _expected_glyph_count: u16,
    ) {
    }

    /// Grayscale-coverage rasterization is a macOS-only recovery path; off macOS
    /// there is no CoreText, so this always declines.
    pub fn rasterize_coverage_glyph(
        &self,
        _face: viso_text::FontFaceId,
        _glyph: u16,
        _pixels_per_em: u16,
    ) -> Option<viso_text::CoverageBitmap> {
        None
    }
}

#[cfg(not(target_os = "macos"))]
impl viso_text::ColorGlyphRasterizer for CoreTextColorRaster {
    fn rasterize_color_glyph(
        &self,
        _face: viso_text::FontFaceId,
        _glyph: u16,
        _pixels_per_em: u16,
    ) -> Option<viso_text::ColorGlyph> {
        None
    }
}

/// CoreText color-emoji rasterizer: the macOS binding behind
/// [`viso_text::ColorGlyphRasterizer`].
///
/// A system emoji face reaches viso-text with its color strikes stripped (see
/// [`system_fonts`](self)), so ttf-parser cannot draw its glyphs. This module
/// hands the glyph back to CoreText — re-opening the face by its PostScript name
/// and rasterizing with `CTFontDrawGlyphs` into a premultiplied RGBA bitmap the
/// color atlas packs directly.
///
/// The bitmap remains **premultiplied**: the color-glyph GPU path lowers to an
/// image draw with a white tint, which passes the texel through unchanged. The
/// conversion therefore only swizzles BGRA to RGBA.
#[cfg(target_os = "macos")]
mod color {
    use std::cell::RefCell;
    use std::collections::HashMap;
    use std::ptr::NonNull;

    use objc2_core_foundation::{CFRetained, CGFloat, CGPoint, CGRect};
    use objc2_core_graphics::{
        CGBitmapContextCreate, CGBitmapContextGetBytesPerRow, CGBitmapContextGetData, CGColorSpace,
        CGContext,
    };
    use objc2_core_text::{CTFont, CTFontOrientation};

    use viso_text::{ColorGlyph, ColorGlyphRasterizer, FontFaceId};

    /// CoreText raster info flags: premultiplied-first alpha byte-ordered
    /// little-endian, i.e. the context's memory layout is `[B, G, R, A]` with the
    /// color channels premultiplied by alpha. `PremultipliedFirst = 2`,
    /// `ByteOrder32Little = 0x2000`.
    const BITMAP_INFO: u32 = 2 | 0x2000;

    /// Rasterize color glyphs by handing them back to CoreText, keyed on the
    /// resolver's [`FontFaceId`]. The trait method no longer carries a PostScript
    /// name or glyph count, so the facade [`register`](Self::register_face)s each
    /// resolved emoji face's `(ps_name, expected_glyph_count)` once; from then on
    /// a `FontFaceId` names it. Per-ppem `CTFont`s are cached under each face; the
    /// face is re-opened by name on the first miss. Cold path — one `dyn` call per
    /// uncached color glyph — so interior-mutable `RefCell`/`HashMap` is fine
    /// (see AGENTS.md section 42).
    pub struct CoreTextColorRaster {
        /// Shared registry of live `CTFont` handles the provider resolved. A face
        /// is rasterized through the exact handle CoreText picked during the
        /// cascade, looked up by the same PostScript name the binding carries.
        live: super::LiveFontRegistry,
        /// Facade-supplied binding from a resolved face id to the PostScript name
        /// its live handle is registered under.
        bindings: RefCell<HashMap<FontFaceId, String>>,
        /// Per-face caches, populated on the first glyph of each face.
        faces: RefCell<HashMap<FontFaceId, FaceCache>>,
    }

    /// A face's resolved base handle plus its per-ppem size copies.
    struct FaceCache {
        /// The live base handle from the registry, at whatever size it was
        /// resolved; each ppem instance is a size-copy of it.
        base: CFRetained<CTFont>,
        /// Per-ppem instances (`CTFontCreateCopyWithAttributes` at each size).
        by_ppem: HashMap<u16, CFRetained<CTFont>>,
    }

    impl CoreTextColorRaster {
        pub fn new(live: super::LiveFontRegistry) -> Self {
            Self {
                live,
                bindings: RefCell::new(HashMap::new()),
                faces: RefCell::new(HashMap::new()),
            }
        }

        /// Bind a resolved face's id to the PostScript name its live handle is
        /// registered under. Called by the facade once per resolved color /
        /// proprietary-outline face, before rasterizing its glyphs. The glyph
        /// count is no longer needed — the live handle is authoritative.
        pub fn register_face(&self, face: FontFaceId, ps_name: &str, _expected_glyph_count: u16) {
            self.bindings.borrow_mut().insert(face, ps_name.to_string());
        }
    }

    impl CoreTextColorRaster {
        /// Resolve and cache the per-ppem `CTFont` a registered face rasterizes
        /// through. Shared by the color and grayscale-coverage raster paths.
        ///
        /// The base handle is the live `CTFont` the provider resolved (looked up
        /// by the face's registered PostScript name), copied to `ppem` with
        /// `CTFontCreateCopyWithAttributes` — no `CTFontCreateWithName`, so the
        /// private `.`-prefixed system names that break name lookup never matter.
        fn font_for(&self, face: FontFaceId, ppem: u16) -> Option<CFRetained<CTFont>> {
            let mut faces = self.faces.borrow_mut();
            let entry = match faces.get_mut(&face) {
                Some(e) => e,
                None => {
                    let ps_name = self.bindings.borrow().get(&face)?.clone();
                    let base = self.live.get(&ps_name)?;
                    faces.entry(face).or_insert(FaceCache {
                        base,
                        by_ppem: HashMap::new(),
                    })
                }
            };
            let font = match entry.by_ppem.get(&ppem) {
                Some(f) => f.clone(),
                None => {
                    // SAFETY: CoreText FFI. A null matrix/descriptor asks for a
                    // plain size copy of the live base handle; the result is
                    // returned retained (CFRetained releases it).
                    let f = unsafe {
                        entry
                            .base
                            .copy_with_attributes(ppem as CGFloat, std::ptr::null(), None)
                    };
                    entry.by_ppem.insert(ppem, f.clone());
                    f
                }
            };
            Some(font)
        }

        /// Rasterize a registered face's glyph to grayscale A8 coverage through
        /// CoreText, for faces whose outlines a generic parser cannot render —
        /// Apple's proprietary `hvgl` (PingFang, `.SFNS`-fallback CJK) has no
        /// `glyf`/`CFF`, so `ttf-parser` produces an empty bitmap and the glyph
        /// silently vanishes. Drawing through CoreText into an alpha-only context
        /// recovers the coverage the A8 atlas expects. The face must have been
        /// [`register_face`](Self::register_face)d first.
        pub fn rasterize_coverage_glyph(
            &self,
            face: FontFaceId,
            glyph: u16,
            pixels_per_em: u16,
        ) -> Option<viso_text::CoverageBitmap> {
            let ppem = clamp_ppem(pixels_per_em);
            let font = self.font_for(face, ppem)?;
            rasterize_coverage_glyph(&font, glyph)
        }
    }

    impl ColorGlyphRasterizer for CoreTextColorRaster {
        fn rasterize_color_glyph(
            &self,
            face: FontFaceId,
            glyph: u16,
            pixels_per_em: u16,
        ) -> Option<ColorGlyph> {
            let ppem = clamp_ppem(pixels_per_em);
            let font = self.font_for(face, ppem)?;
            rasterize_glyph(&font, glyph, ppem)
        }
    }

    /// Clamp a pixels-per-em request to a sane strike range so a runaway size
    /// cannot ask CoreText for a giant bitmap.
    fn clamp_ppem(pixels_per_em: u16) -> u16 {
        pixels_per_em.clamp(1, 512)
    }

    /// Rasterize `glyph_id` from `font` at `ppem` into a premultiplied RGBA
    /// [`ColorGlyph`], mirroring the reference: measure the y-up ink bbox, floor
    /// the origin and ceil the extent to integer pixels, draw into a BGRA
    /// premultiplied bitmap at identity CTM, then swizzle BGRA→RGBA (keeping the
    /// premultiplied alpha; see the module divergence note).
    fn rasterize_glyph(font: &CTFont, glyph_id: u16, ppem: u16) -> Option<ColorGlyph> {
        let glyph = glyph_id;
        let mut bbox = CGRect::default();
        // SAFETY: CoreText FFI. `glyph` and `bbox` are single-element buffers; the
        // count of 1 matches, and the pointers are valid for the call.
        let _ = unsafe {
            font.bounding_rects_for_glyphs(
                CTFontOrientation::Horizontal,
                NonNull::from(&glyph),
                &mut bbox,
                1,
            )
        };

        if !bbox.origin.x.is_finite()
            || !bbox.origin.y.is_finite()
            || !bbox.size.width.is_finite()
            || !bbox.size.height.is_finite()
        {
            return None;
        }

        let x0 = bbox.origin.x.floor();
        let y0 = bbox.origin.y.floor();
        let w = ((bbox.origin.x + bbox.size.width).ceil() - x0) as i32;
        let h = ((bbox.origin.y + bbox.size.height).ceil() - y0) as i32;
        if w <= 0 || h <= 0 {
            return None;
        }
        let (w, h) = (w as usize, h as usize);

        let space = CGColorSpace::new_device_rgb()?;
        // SAFETY: CoreText/CoreGraphics FFI. A null data pointer asks CG to
        // allocate the backing store (freed with the context); `space` is a valid
        // device-RGB space that outlives the context.
        let ctx = unsafe {
            CGBitmapContextCreate(std::ptr::null_mut(), w, h, 8, 0, Some(&space), BITMAP_INFO)
        }?;

        let pos = CGPoint {
            x: -x0 as CGFloat,
            y: -y0 as CGFloat,
        };
        // SAFETY: CoreText FFI. Single-element glyph/position buffers matching the
        // count of 1; `ctx` is the bitmap context just created.
        unsafe {
            font.draw_glyphs(NonNull::from(&glyph), NonNull::from(&pos), 1, &ctx);
        }

        let rgba = read_back_rgba(&ctx, w, h)?;
        Some(ColorGlyph {
            width: w as u32,
            height: h as u32,
            rgba,
            pixels_per_em: ppem,
            // The strike origin offset from the pen, in strike pixels: the atlas
            // scales it by `dpx_per_em / pixels_per_em` when placing the quad.
            origin_px: [x0 as f32, y0 as f32],
        })
    }

    /// `kCGImageAlphaOnly`: an 8-bit, single-component (alpha) bitmap with no
    /// color channels and no color space. Drawing an opaque glyph into it leaves
    /// exactly the ink coverage in each byte — the A8 mask the coverage atlas
    /// wants — with no premultiply or swizzle to undo.
    const ALPHA_ONLY: u32 = 7;

    /// Rasterize `glyph_id` from `font` to grayscale A8 coverage: measure the
    /// y-up ink bbox, draw the glyph opaque into an alpha-only context, and read
    /// the coverage bytes back tightly packed. Mirrors [`rasterize_glyph`]'s
    /// geometry (floor origin, ceil extent, identity CTM, `-x0/-y0` pen) so a
    /// face routed here places identically to one that went through `glyf`/`CFF`.
    ///
    /// This is the recovery path for Apple proprietary-outline faces (`hvgl`,
    /// e.g. PingFang) that carry no parser-readable outline: `ttf-parser` returns
    /// an empty bitmap for them, so without this the glyph draws nothing.
    fn rasterize_coverage_glyph(font: &CTFont, glyph_id: u16) -> Option<viso_text::CoverageBitmap> {
        let glyph = glyph_id;
        let mut bbox = CGRect::default();
        // SAFETY: CoreText FFI. Single-element glyph/bbox buffers matching count 1.
        let _ = unsafe {
            font.bounding_rects_for_glyphs(
                CTFontOrientation::Horizontal,
                NonNull::from(&glyph),
                &mut bbox,
                1,
            )
        };
        if !bbox.origin.x.is_finite()
            || !bbox.origin.y.is_finite()
            || !bbox.size.width.is_finite()
            || !bbox.size.height.is_finite()
        {
            return None;
        }
        let x0 = bbox.origin.x.floor();
        let y0 = bbox.origin.y.floor();
        let w = ((bbox.origin.x + bbox.size.width).ceil() - x0) as i32;
        let h = ((bbox.origin.y + bbox.size.height).ceil() - y0) as i32;
        if w <= 0 || h <= 0 {
            return None;
        }
        let (w, h) = (w as usize, h as usize);

        // Alpha-only context: no color space, one coverage byte per pixel.
        // SAFETY: CoreGraphics FFI. A null data pointer asks CG to allocate the
        // backing store (freed with the context); alpha-only takes a null space.
        let ctx =
            unsafe { CGBitmapContextCreate(std::ptr::null_mut(), w, h, 8, 0, None, ALPHA_ONLY) }?;

        // Draw the glyph fully opaque so each alpha byte is its ink coverage.
        CGContext::set_gray_fill_color(Some(&ctx), 1.0, 1.0);
        let pos = CGPoint {
            x: -x0 as CGFloat,
            y: -y0 as CGFloat,
        };
        // SAFETY: CoreText FFI. Single-element glyph/position buffers, count 1;
        // `ctx` is the alpha-only bitmap context just created.
        unsafe {
            font.draw_glyphs(NonNull::from(&glyph), NonNull::from(&pos), 1, &ctx);
        }

        let coverage = read_back_alpha(&ctx, w, h)?;
        Some(viso_text::CoverageBitmap {
            width: w as u32,
            height: h as u32,
            // y-up bbox: left edge is x0; top edge (y-up) is y0 + h.
            left: x0 as f32,
            top: (y0 + h as f64) as f32,
            coverage,
        })
    }

    /// Read an alpha-only context's coverage bytes back tightly packed. Like
    /// [`read_back_rgba`], the source stride is CG's padded
    /// `CGBitmapContextGetBytesPerRow`, not a tight `w`.
    fn read_back_alpha(ctx: &CGContext, w: usize, h: usize) -> Option<Vec<u8>> {
        let data = CGBitmapContextGetData(Some(ctx));
        if data.is_null() {
            return None;
        }
        let stride = CGBitmapContextGetBytesPerRow(Some(ctx));
        if stride < w {
            return None;
        }
        let mut coverage = vec![0u8; w * h];
        for y in 0..h {
            // SAFETY: the backing store holds `stride >= w` bytes per row for `h`
            // rows, so `data + y * stride` points to `w` valid source bytes.
            let row = unsafe { std::slice::from_raw_parts(data.add(y * stride) as *const u8, w) };
            coverage[y * w..(y + 1) * w].copy_from_slice(row);
        }
        Some(coverage)
    }

    /// Read the context's BGRA premultiplied pixels back and swizzle to RGBA,
    /// keeping the premultiplied alpha (Viso's image path expects premultiplied).
    ///
    /// `CGBitmapContextCreate` was called with `bytes_per_row = 0`, which asks
    /// CoreGraphics to pick its own row stride — and it aligns that stride up
    /// (commonly to a 16/32/64-byte multiple), so for many glyph widths the
    /// backing store's row stride is *larger* than a tight `w * 4`. The source
    /// must therefore be walked row-by-row at the context's real
    /// `CGBitmapContextGetBytesPerRow` stride; only the tight-packed `w * 4`
    /// output the atlas expects is produced here. (Assuming a tight source stride
    /// skewed every row after the first, yielding garbage/transparent bitmaps.)
    fn read_back_rgba(ctx: &CGContext, w: usize, h: usize) -> Option<Vec<u8>> {
        let data = CGBitmapContextGetData(Some(ctx));
        if data.is_null() {
            return None;
        }
        let stride = CGBitmapContextGetBytesPerRow(Some(ctx));
        if stride < w * 4 {
            return None;
        }
        let mut rgba = vec![0u8; w * h * 4];
        for y in 0..h {
            // SAFETY: the backing store holds `stride` bytes per row for `h` rows,
            // so `data + y * stride` points to `w * 4` valid source bytes (the row
            // is at least `w * 4` wide, checked above).
            let row =
                unsafe { std::slice::from_raw_parts(data.add(y * stride) as *const u8, w * 4) };
            let dst_row = &mut rgba[y * w * 4..(y + 1) * w * 4];
            let (dst_px, _) = dst_row.as_chunks_mut::<4>();
            let (src_px, _) = row.as_chunks::<4>();
            for (dst, px) in dst_px.iter_mut().zip(src_px) {
                // Source is [B, G, R, A]; write [R, G, B, A].
                dst[0] = px[2];
                dst[1] = px[1];
                dst[2] = px[0];
                dst[3] = px[3];
            }
        }
        Some(rgba)
    }

    #[cfg(test)]
    mod tests {
        use objc2_core_foundation::CGFloat;
        use objc2_core_text::CTFont;

        use super::*;

        /// The color-glyph readback must return real, opaque emoji pixels — not a
        /// transparent or skewed bitmap. This drives the CoreText raster path
        /// (`CTFontCreateCopyWithAttributes` → `CTFontDrawGlyphs` → `read_back_rgba`)
        /// against the live system emoji face and asserts some pixel is meaningfully
        /// opaque. It is the guard for the stride bug: `read_back_rgba` must walk the
        /// source at `CGBitmapContextGetBytesPerRow` (CG pads it), not at a tight
        /// `w * 4`; the tight assumption produced fully-transparent/garbage rows that
        /// this assertion would catch.
        #[test]
        fn color_glyph_readback_has_opaque_pixels() {
            // Open the well-known system emoji face directly and seed the shared
            // registry with the live handle, exactly as the provider does during a
            // real cascade. The raster then copies this handle to each ppem.
            let cf = objc2_core_foundation::CFString::from_str("AppleColorEmoji");
            // SAFETY: CoreText FFI. Returns a retained font; the name outlives the
            // call and a null matrix means identity.
            let font = unsafe { CTFont::with_name(&cf, 16.0 as CGFloat, std::ptr::null()) };
            // SAFETY: CoreText FFI, reads the glyph count of a live font.
            let count = unsafe { font.glyph_count() };
            assert!(count > 0, "AppleColorEmoji must resolve on macOS");

            let live = super::super::LiveFontRegistry::new();
            live.insert("AppleColorEmoji".to_string(), font);
            let raster = CoreTextColorRaster::new(live);
            let face = FontFaceId(0);
            raster.register_face(face, "AppleColorEmoji", count as u16);
            // Scan a handful of low glyph ids; at least one is a painted color
            // emoji whose readback must contain an opaque pixel.
            let mut best_alpha = 0u8;
            for glyph_id in 1u16..40 {
                if let Some(g) = raster.rasterize_color_glyph(face, glyph_id, 64) {
                    assert_eq!(
                        g.rgba.len(),
                        g.width as usize * g.height as usize * 4,
                        "readback must be tightly packed w*h*4 regardless of CG's padded stride"
                    );
                    let max_a = g
                        .rgba
                        .as_chunks::<4>()
                        .0
                        .iter()
                        .map(|px| px[3])
                        .max()
                        .unwrap_or(0);
                    best_alpha = best_alpha.max(max_a);
                    if best_alpha > 200 {
                        break;
                    }
                }
            }
            assert!(
                best_alpha > 200,
                "a scanned color emoji glyph must read back with an opaque pixel \
                 (got max alpha {best_alpha}); a tight-stride readback yields \
                 transparent/garbage rows"
            );
        }
    }
}

#[cfg(target_os = "macos")]
mod imp {
    use objc2_core_foundation::{CFIndex, CFRange, CFRetained, CFString};
    use objc2_core_text::{CTFont, CTFontTableOptions, CTFontTableTag, CTFontUIFontType};

    use viso_text::{FontRole, SystemFontProvider, SystemFontQuery, SystemFontResult};

    /// Resolve system fonts through CoreText. The negative cache lives in
    /// `viso_text::SystemFallback`; this performs the OS query and stashes the
    /// live `CTFont` handle it resolved into the shared [`LiveFontRegistry`], so
    /// the color/coverage raster rasterizes through that exact handle instead of
    /// re-deriving it from an unresolvable private PostScript name.
    ///
    /// [`LiveFontRegistry`]: super::LiveFontRegistry
    pub struct CoreTextProvider {
        live: super::LiveFontRegistry,
    }

    impl CoreTextProvider {
        pub fn new(live: super::LiveFontRegistry) -> Self {
            Self { live }
        }
    }

    impl SystemFontProvider for CoreTextProvider {
        fn resolve_system_face(&self, query: &SystemFontQuery) -> Option<SystemFontResult> {
            let (bytes, postscript_name, covering) = load_system_font(query)?;
            // Stash the exact handle CoreText resolved, keyed by the authoritative
            // PostScript name the raster binds against. The first covering face for
            // a name wins; identical later resolutions no-op.
            if let Some(name) = &postscript_name {
                self.live.insert(name.clone(), covering);
            }
            Some(SystemFontResult {
                bytes,
                index: 0,
                postscript_name,
            })
        }
    }

    /// A glyph count at or below this is the LastResort font (tofu boxes): it has
    /// only the handful of "missing glyph" outlines. Rejecting it here is what
    /// keeps the shaper's missing-script scan re-firing instead of settling on
    /// boxes with a non-zero glyph id.
    const LAST_RESORT_MAX_GLYPHS: CFIndex = 16;

    fn is_last_resort(glyph_count: CFIndex) -> bool {
        glyph_count <= LAST_RESORT_MAX_GLYPHS
    }

    /// Query CoreText for a face covering `query`'s sample string, reject the
    /// LastResort fallback, and reassemble its sfnt bytes for ttf-parser. Also
    /// returns the face's CoreText PostScript name: the synthesized sfnt may carry
    /// only Macintosh-platform `name` records that a generic reader cannot decode,
    /// so the color/proprietary-outline raster re-opens the face under *this* name
    /// rather than one parsed back out of the bytes.
    fn load_system_font(
        query: &SystemFontQuery,
    ) -> Option<(Vec<u8>, Option<String>, CFRetained<CTFont>)> {
        let ui_type = match query.role {
            FontRole::Ui | FontRole::Cjk => CTFontUIFontType::System,
            FontRole::Serif => CTFontUIFontType::System,
            FontRole::Mono => CTFontUIFontType::UserFixedPitch,
            // Emoji has no dedicated UI type; start from the system font and let
            // `for_string` cascade to the color-emoji face via the sample.
            FontRole::Emoji => CTFontUIFontType::System,
        };

        let language = if query.lang.is_empty() {
            None
        } else {
            Some(CFString::from_str(&query.lang))
        };

        // SAFETY: CoreText FFI. `new_ui_font_for_language` allocates a font and
        // returns it retained (CFRetained drops it); the language string, when
        // present, outlives the call.
        let base = unsafe { CTFont::new_ui_font_for_language(ui_type, 0.0, language.as_deref()) }?;

        // The base UI font already covers Latin; for CJK/emoji we ask CoreText
        // which face renders the sample and let it cascade there.
        let sample = CFString::from_str(&query.sample);
        let range = CFRange {
            location: 0,
            length: query.sample.encode_utf16().count() as CFIndex,
        };
        // SAFETY: CoreText FFI. `for_string` returns a retained font covering the
        // range; `sample` outlives the call.
        let covering = unsafe { base.for_string(&sample, range) };

        // SAFETY: CoreText FFI, reads the glyph count of a live font.
        let glyph_count = unsafe { covering.glyph_count() };
        if is_last_resort(glyph_count) {
            return None;
        }

        let bytes = sfnt_bytes_from_ctfont(&covering)?;
        let ps_name = postscript_name(&covering);
        Some((bytes, ps_name, covering))
    }

    /// Copy a CoreText font's PostScript name to an owned `String`. This is the
    /// name the platform raster re-opens the face under, and it is authoritative:
    /// a reassembled sfnt often keeps only Macintosh-platform `name` records that
    /// `ttf-parser` returns `None` for, so the color/`hvgl` face would otherwise
    /// never register.
    fn postscript_name(font: &CTFont) -> Option<String> {
        // SAFETY: CoreText FFI. Returns a retained CFString (or null) that the
        // CFRetained wrapper releases on drop.
        let name = unsafe { font.post_script_name() };
        Some(name.to_string())
    }

    /// Color-bitmap tables we never copy: they are huge (Apple's `sbix` is
    /// ~179 MB) and ttf-parser cannot decode Apple's private strike format
    /// anyway. The emoji face is flagged [`is_color_emoji`] and its color glyphs
    /// are rasterized on demand by [`CoreTextColorRaster`], which hands the glyph
    /// back to CoreText keyed on the face's PostScript name.
    ///
    /// [`is_color_emoji`]: viso_text::FontFace::is_color_emoji
    /// [`CoreTextColorRaster`]: super::CoreTextColorRaster
    const SKIP_TAGS: &[u32] = &[
        tag(b"sbix"),
        tag(b"CBDT"),
        tag(b"CBLC"),
        tag(b"COLR"),
        tag(b"CPAL"),
    ];

    /// Variable-font tables dropped when a `glyf` table is present: ttf-parser
    /// cannot resolve Apple `gvar` deltas, so they would yield blank glyphs. We
    /// only ever consume the default instance.
    const VAR_TAGS: &[u32] = &[
        tag(b"gvar"),
        tag(b"fvar"),
        tag(b"avar"),
        tag(b"HVAR"),
        tag(b"MVAR"),
        tag(b"STAT"),
    ];

    const GLYF_TAG: u32 = tag(b"glyf");

    /// Big-endian four-character-code, matching the sfnt on-disk tag encoding.
    const fn tag(b: &[u8; 4]) -> u32 {
        u32::from_be_bytes(*b)
    }

    /// Walk a CoreText font's table directory, copy the tables worth keeping, and
    /// reassemble them into an sfnt byte blob ttf-parser can parse.
    fn sfnt_bytes_from_ctfont(font: &CTFont) -> Option<Vec<u8>> {
        // SAFETY: CoreText FFI. Returns a retained array of table tags.
        let tables = unsafe { font.available_tables(CTFontTableOptions::empty()) }?;

        let count = tables.count();
        let mut tags: Vec<u32> = Vec::with_capacity(count as usize);
        for i in 0..count {
            // The CFArray stores each tag *as* the pointer slot value — the
            // integer tag is stuffed directly into the `const void*`, not boxed
            // as a CFNumber. Unboxing it would segfault; reinterpret the pointer
            // bits as the tag instead.
            // SAFETY: FFI, in-bounds index; the "pointer" is the tag value.
            let value = unsafe { tables.value_at_index(i) };
            tags.push(value as usize as u32);
        }

        let has_glyf = tags.contains(&GLYF_TAG);

        let mut copied: Vec<(u32, Vec<u8>)> = Vec::with_capacity(tags.len());
        for &t in &tags {
            if SKIP_TAGS.contains(&t) {
                continue;
            }
            if has_glyf && VAR_TAGS.contains(&t) {
                continue;
            }
            // SAFETY: CoreText FFI. Copies the table's bytes into a retained
            // CFData; `t` is a tag we just read from this font.
            let Some(data) =
                (unsafe { font.table(t as CTFontTableTag, CTFontTableOptions::empty()) })
            else {
                continue;
            };
            let len = data.length() as usize;
            // SAFETY: `byte_ptr()` returns a pointer to `length()` valid bytes
            // owned by the retained CFData, so the slice is in-bounds for `len`.
            let slice = unsafe { std::slice::from_raw_parts(data.byte_ptr(), len) };
            copied.push((t, slice.to_vec()));
        }

        if copied.is_empty() {
            return None;
        }

        Some(assemble_sfnt(copied))
    }

    /// Assemble copied tables into a valid sfnt container: sorted directory,
    /// 4-byte-aligned table data, correct search-range header fields. Pure and
    /// portable — the CoreText-facing code above hands it owned bytes.
    fn assemble_sfnt(mut tables: Vec<(u32, Vec<u8>)>) -> Vec<u8> {
        // sfnt requires the table directory sorted by tag ascending.
        tables.sort_by_key(|(t, _)| *t);

        let has_cff = tables
            .iter()
            .any(|(t, _)| *t == tag(b"CFF ") || *t == tag(b"CFF2"));
        // 'OTTO' for CFF-outline fonts, 0x00010000 for TrueType.
        let sfnt_version: u32 = if has_cff { 0x4F54_544F } else { 0x0001_0000 };

        let num_tables = tables.len() as u16;
        // searchRange = (largest power of two <= numTables) * 16.
        let mut entry_selector = 0u16;
        while (1u16 << (entry_selector + 1)) <= num_tables {
            entry_selector += 1;
        }
        let search_range = (1u16 << entry_selector) * 16;
        let range_shift = num_tables * 16 - search_range;

        let header_len = 12;
        let dir_len = tables.len() * 16;
        let mut data_len = 0usize;
        for (_, bytes) in &tables {
            data_len += align4(bytes.len());
        }
        let mut out = vec![0u8; header_len + dir_len + data_len];

        // Offset table (header).
        out[0..4].copy_from_slice(&sfnt_version.to_be_bytes());
        out[4..6].copy_from_slice(&num_tables.to_be_bytes());
        out[6..8].copy_from_slice(&search_range.to_be_bytes());
        out[8..10].copy_from_slice(&entry_selector.to_be_bytes());
        out[10..12].copy_from_slice(&range_shift.to_be_bytes());

        // Directory records + table data. Leave every checkSum field zero.
        let head_tag = tag(b"head");
        let mut data_offset = header_len + dir_len;
        for (i, (t, bytes)) in tables.iter().enumerate() {
            let rec = header_len + i * 16;
            out[rec..rec + 4].copy_from_slice(&t.to_be_bytes());
            // checkSum at rec+4..rec+8 stays zero.
            out[rec + 8..rec + 12].copy_from_slice(&(data_offset as u32).to_be_bytes());
            out[rec + 12..rec + 16].copy_from_slice(&(bytes.len() as u32).to_be_bytes());

            out[data_offset..data_offset + bytes.len()].copy_from_slice(bytes);
            // Zero head.checkSumAdjustment (offset 8 within the head table) so a
            // reader that does validate sees the canonical zeroed field.
            if *t == head_tag && bytes.len() >= 12 {
                for b in out.iter_mut().skip(data_offset + 8).take(4) {
                    *b = 0;
                }
            }
            data_offset += align4(bytes.len());
        }

        out
    }

    /// Round `n` up to the next multiple of four (sfnt tables are 4-byte aligned).
    fn align4(n: usize) -> usize {
        (n + 3) & !3
    }

    /// Big-endian sfnt checksum: sum of the table's 32-bit words, zero-padding a
    /// trailing partial word. Only used by unit tests — the assembler leaves
    /// checksums zero on purpose (ttf-parser does not verify them).
    #[cfg(test)]
    fn sfnt_checksum(bytes: &[u8]) -> u32 {
        let mut sum = 0u32;
        let mut i = 0;
        while i < bytes.len() {
            let mut word = [0u8; 4];
            let take = (bytes.len() - i).min(4);
            word[..take].copy_from_slice(&bytes[i..i + take]);
            sum = sum.wrapping_add(u32::from_be_bytes(word));
            i += 4;
        }
        sum
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn detects_last_resort_by_glyph_count() {
            // The tofu font has only the missing-glyph outlines; a real face has
            // many. The threshold rejects the former without touching outlines.
            assert!(is_last_resort(0));
            assert!(is_last_resort(16));
            assert!(!is_last_resort(17));
            assert!(!is_last_resort(65_535));
        }

        #[test]
        fn checksum_zero_pads_partial_word() {
            // A three-byte tail is summed as if padded to four bytes with zeros,
            // matching the sfnt spec — not truncated or read out of bounds.
            let three = sfnt_checksum(&[0x00, 0x00, 0x01]);
            let padded = sfnt_checksum(&[0x00, 0x00, 0x01, 0x00]);
            assert_eq!(three, padded);
            assert_eq!(three, 0x0000_0100);
        }

        #[test]
        fn assembles_valid_sorted_aligned_directory() {
            // Two tables handed in reverse tag order with unaligned lengths must
            // come out sorted, 4-byte aligned, with a coherent search-range header
            // and offsets/lengths that round-trip through ttf-parser's directory.
            let head = {
                // A minimal 54-byte head table with a nonzero checkSumAdjustment
                // that the assembler must zero.
                let mut h = vec![0u8; 54];
                h[8..12].copy_from_slice(&0xDEAD_BEEFu32.to_be_bytes());
                h
            };
            let cmap = vec![1u8, 2, 3]; // length 3 → must pad to 4.

            let tables = vec![(tag(b"head"), head.clone()), (tag(b"cmap"), cmap.clone())];
            let sfnt = assemble_sfnt(tables);

            // Header: numTables = 2.
            assert_eq!(u16::from_be_bytes([sfnt[4], sfnt[5]]), 2);
            // TrueType version (no CFF present).
            assert_eq!(
                u32::from_be_bytes([sfnt[0], sfnt[1], sfnt[2], sfnt[3]]),
                0x0001_0000
            );

            // Directory sorted ascending: 'cmap' (0x636D6170) before 'head'.
            let rec0_tag = u32::from_be_bytes([sfnt[12], sfnt[13], sfnt[14], sfnt[15]]);
            let rec1_tag = u32::from_be_bytes([sfnt[28], sfnt[29], sfnt[30], sfnt[31]]);
            assert_eq!(rec0_tag, tag(b"cmap"));
            assert_eq!(rec1_tag, tag(b"head"));

            // Every table offset is 4-byte aligned.
            let off0 = u32::from_be_bytes([sfnt[20], sfnt[21], sfnt[22], sfnt[23]]) as usize;
            let off1 = u32::from_be_bytes([sfnt[36], sfnt[37], sfnt[38], sfnt[39]]) as usize;
            assert_eq!(off0 % 4, 0);
            assert_eq!(off1 % 4, 0);

            // head.checkSumAdjustment (head offset + 8) was zeroed.
            let head_off = off1;
            assert_eq!(
                u32::from_be_bytes([
                    sfnt[head_off + 8],
                    sfnt[head_off + 9],
                    sfnt[head_off + 10],
                    sfnt[head_off + 11],
                ]),
                0,
            );

            // ttf-parser can at least walk the reassembled directory.
            let raw = ttf_parser::RawFace::parse(&sfnt, 0);
            assert!(raw.is_ok(), "reassembled sfnt directory must parse");
        }
    }
}
