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
//! (`CTFontCopyTable`), and rebuilds a valid sfnt container. Two hazards, both
//! learned from the makepad reference and reproduced here:
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
    pub fn new() -> Self {
        Self
    }
}

#[cfg(not(target_os = "macos"))]
impl Default for CoreTextProvider {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(not(target_os = "macos"))]
impl viso_text::SystemFontProvider for CoreTextProvider {
    fn load(&self, _query: &viso_text::SystemFontQuery) -> Option<viso_text::SystemFontResult> {
        None
    }
}

#[cfg(target_os = "macos")]
pub use color::CoreTextColorRaster;

/// A no-op color rasterizer for platforms without a native binding. Always
/// declines, so a color-emoji face yields no color glyph and the shaper falls
/// through to the outline path (no color output, no panic).
#[cfg(not(target_os = "macos"))]
pub struct CoreTextColorRaster;

#[cfg(not(target_os = "macos"))]
impl CoreTextColorRaster {
    pub fn new() -> Self {
        Self
    }
}

#[cfg(not(target_os = "macos"))]
impl Default for CoreTextColorRaster {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(not(target_os = "macos"))]
impl viso_text::ColorGlyphRasterizer for CoreTextColorRaster {
    fn rasterize(
        &self,
        _ps_name: &str,
        _expected_glyph_count: u16,
        _glyph_id: u16,
        _dpx_per_em: f32,
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
/// ## Divergence from the reference
///
/// The makepad reference un-premultiplies the CoreText bitmap into straight
/// alpha (its atlas stores straight coverage). Viso keeps the bitmap
/// **premultiplied**: the color-glyph GPU path lowers to an image draw with a
/// white tint (`texel * tint` with `tint = [1,1,1,1]`), which passes a
/// premultiplied texel through unchanged. So we only swizzle BGRA→RGBA and never
/// un-premultiply.
#[cfg(target_os = "macos")]
mod color {
    use std::cell::RefCell;
    use std::collections::HashMap;
    use std::ptr::NonNull;

    use objc2_core_foundation::{CFRetained, CFString, CGFloat, CGPoint, CGRect};
    use objc2_core_graphics::{
        CGBitmapContextCreate, CGBitmapContextGetData, CGColorSpace, CGContext,
    };
    use objc2_core_text::{CTFont, CTFontOrientation};

    use viso_text::{ColorGlyph, ColorGlyphRasterizer};

    /// CoreText raster info flags: premultiplied-first alpha byte-ordered
    /// little-endian, i.e. the context's memory layout is `[B, G, R, A]` with the
    /// color channels premultiplied by alpha. `PremultipliedFirst = 2`,
    /// `ByteOrder32Little = 0x2000`.
    const BITMAP_INFO: u32 = 2 | 0x2000;

    /// Rasterize color glyphs by handing them back to CoreText. Holds a per-ppem
    /// `CTFont` cache keyed on the quantized pixels-per-em; the face is re-opened
    /// by PostScript name on the first miss. Cold path — one `dyn` call per
    /// uncached color glyph — so interior-mutable `RefCell`/`HashMap` is fine
    /// (see AGENTS.md section 42).
    pub struct CoreTextColorRaster {
        /// Per-requested-PostScript-name face caches, populated on the first color
        /// glyph of each emoji face.
        faces: RefCell<HashMap<String, FaceCache>>,
    }

    /// The resolved font name a requested PostScript name binds to, plus that
    /// face's per-ppem `CTFont` instances.
    struct FaceCache {
        /// The candidate name that matched the expected glyph count — reused when
        /// creating each per-ppem instance so all sizes bind the same face.
        resolved_name: String,
        /// Per-ppem instances (`CTFontCreateWithName` at each size).
        by_ppem: HashMap<u16, CFRetained<CTFont>>,
    }

    impl CoreTextColorRaster {
        pub fn new() -> Self {
            Self {
                faces: RefCell::new(HashMap::new()),
            }
        }
    }

    impl Default for CoreTextColorRaster {
        fn default() -> Self {
            Self::new()
        }
    }

    impl ColorGlyphRasterizer for CoreTextColorRaster {
        fn rasterize(
            &self,
            ps_name: &str,
            expected_glyph_count: u16,
            glyph_id: u16,
            dpx_per_em: f32,
        ) -> Option<ColorGlyph> {
            let ppem = quantize_ppem(dpx_per_em);

            let mut faces = self.faces.borrow_mut();
            // Resolve the candidate name for this face once (probing against the
            // expected glyph count), then cache a CTFont per ppem under it.
            let entry = match faces.get_mut(ps_name) {
                Some(e) => e,
                None => {
                    let resolved_name = resolve_name(ps_name, expected_glyph_count)?;
                    faces.entry(ps_name.to_string()).or_insert(FaceCache {
                        resolved_name,
                        by_ppem: HashMap::new(),
                    })
                }
            };
            let font = match entry.by_ppem.get(&ppem) {
                Some(f) => f.clone(),
                None => {
                    let f = with_name(&entry.resolved_name, ppem as f64);
                    entry.by_ppem.insert(ppem, f.clone());
                    f
                }
            };
            drop(faces);

            rasterize_glyph(&font, glyph_id, ppem)
        }
    }

    /// Quantize a device-pixels-per-em request to an integer ppem, clamped to a
    /// sane strike range so a runaway size cannot ask CoreText for a giant bitmap.
    fn quantize_ppem(dpx_per_em: f32) -> u16 {
        (dpx_per_em.round() as i32).clamp(1, 512) as u16
    }

    /// Open a font by name via `CTFontCreateWithName` at `size` points.
    fn with_name(name: &str, size: f64) -> CFRetained<CTFont> {
        let cf = CFString::from_str(name);
        // SAFETY: CoreText FFI. `with_name` returns a retained font; the name
        // string outlives the call and the null matrix means identity.
        unsafe { CTFont::with_name(&cf, size as CGFloat, std::ptr::null()) }
    }

    /// Resolve the emoji face's re-open name, mirroring the reference's candidate
    /// probing: try the PostScript name, then the name with a leading `.`
    /// stripped, then a trailing `UI` stripped, then the well-known
    /// `AppleColorEmoji`. Return the first candidate whose glyph count matches the
    /// face viso-text parsed (so we bind the *same* face, not a lookalike).
    fn resolve_name(ps_name: &str, expected_glyph_count: u16) -> Option<String> {
        let mut candidates: Vec<String> = Vec::new();
        let mut push = |c: String| {
            if !c.is_empty() && !c.starts_with('.') && !candidates.contains(&c) {
                candidates.push(c);
            }
        };
        push(ps_name.to_string());
        push(ps_name.trim_start_matches('.').to_string());
        push(ps_name.trim_end_matches("UI").to_string());
        push("AppleColorEmoji".to_string());

        candidates.into_iter().find(|cand| {
            let font = with_name(cand, 16.0);
            // SAFETY: CoreText FFI, reads the glyph count of a live font.
            let count = unsafe { font.glyph_count() };
            count == expected_glyph_count as isize
        })
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

    /// Read the context's BGRA premultiplied pixels back and swizzle to RGBA,
    /// keeping the premultiplied alpha (Viso's image path expects premultiplied).
    fn read_back_rgba(ctx: &CGContext, w: usize, h: usize) -> Option<Vec<u8>> {
        // Returns the context's backing store pointer, valid for `w * h * 4`
        // bytes (bytes-per-row 0 asked for a tight buffer).
        let data = CGBitmapContextGetData(Some(ctx));
        if data.is_null() {
            return None;
        }
        let len = w * h * 4;
        // SAFETY: the backing store holds `len` bytes as established above.
        let src = unsafe { std::slice::from_raw_parts(data as *const u8, len) };
        let mut rgba = vec![0u8; len];
        let (dst_px, _) = rgba.as_chunks_mut::<4>();
        let (src_px, _) = src.as_chunks::<4>();
        for (dst, px) in dst_px.iter_mut().zip(src_px) {
            // Source is [B, G, R, A]; write [R, G, B, A].
            dst[0] = px[2];
            dst[1] = px[1];
            dst[2] = px[0];
            dst[3] = px[3];
        }
        Some(rgba)
    }
}

#[cfg(target_os = "macos")]
mod imp {
    use objc2_core_foundation::{CFIndex, CFRange, CFString};
    use objc2_core_text::{CTFont, CTFontTableOptions, CTFontTableTag, CTFontUIFontType};

    use viso_text::{FontRole, SystemFontProvider, SystemFontQuery, SystemFontResult};

    /// Resolve system fonts through CoreText. Stateless — the negative cache
    /// lives in `viso_text::SystemFallback`, so this only performs the OS query.
    pub struct CoreTextProvider;

    impl CoreTextProvider {
        pub fn new() -> Self {
            Self
        }
    }

    impl Default for CoreTextProvider {
        fn default() -> Self {
            Self::new()
        }
    }

    impl SystemFontProvider for CoreTextProvider {
        fn load(&self, query: &SystemFontQuery) -> Option<SystemFontResult> {
            let bytes = load_system_font(query)?;
            Some(SystemFontResult { bytes, index: 0 })
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
    /// LastResort fallback, and reassemble its sfnt bytes for ttf-parser.
    fn load_system_font(query: &SystemFontQuery) -> Option<Vec<u8>> {
        let ui_type = match query.role {
            FontRole::Ui | FontRole::Cjk => CTFontUIFontType::System,
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

        sfnt_bytes_from_ctfont(&covering)
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
