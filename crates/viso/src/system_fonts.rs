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
//!     AppleColorEmoji's `sbix` table is ~179 MB; we render color emoji through
//!     the bundled Noto face, not the system one, so copying it is pure waste.
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
    /// ~179 MB) and we render color emoji through the bundled Noto face instead.
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
