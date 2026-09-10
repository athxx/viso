//! Typed logical text positions, the UTF-16 bridge, and caret affinity.
//!
//! A text position is never a bare `usize` shared across meanings. A byte
//! offset into UTF-8 source, a UTF-16 code-unit index a platform IME speaks in,
//! a grapheme boundary, a shaping cluster, and a glyph index are distinct types
//! with explicit conversions, so a position from one domain can never be
//! silently used in another. Source text is never implicitly normalized.

/// A byte offset into the UTF-8 logical source text. The default is `0`, the
/// start of any source — matching [`TextPosition::default`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Default)]
pub struct TextOffset(pub usize);

impl TextOffset {
    /// Whether this offset lands on a legal UTF-8 character boundary of `text`.
    /// A caret, selection endpoint, or mapping target must sit on a boundary;
    /// an offset in the interior of a multi-byte scalar is not a valid position.
    /// This is a typed spelling of [`str::is_char_boundary`] so the check reads
    /// as a position contract rather than a raw index test.
    pub fn is_char_boundary(self, text: &str) -> bool {
        text.is_char_boundary(self.0)
    }
}

/// Which side of a boundary a caret belongs to when one logical offset maps to
/// two visual positions — at a BiDi direction boundary or a soft line-wrap.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum CaretAffinity {
    /// Associates with the content before the boundary.
    Upstream,
    /// Associates with the content after the boundary.
    Downstream,
}

/// A logical caret position: a byte offset plus the affinity that disambiguates
/// its visual placement. Selection and hit testing speak in these, never in
/// visual pixels alone.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct TextPosition {
    pub offset: TextOffset,
    pub affinity: CaretAffinity,
}

impl TextPosition {
    /// A position associating with the content before the boundary.
    pub fn upstream(offset: TextOffset) -> Self {
        Self {
            offset,
            affinity: CaretAffinity::Upstream,
        }
    }

    /// A position associating with the content after the boundary.
    pub fn downstream(offset: TextOffset) -> Self {
        Self {
            offset,
            affinity: CaretAffinity::Downstream,
        }
    }
}

/// The composition-local / paragraph-local mapping between UTF-8 byte offsets
/// and the UTF-16 code-unit indices a platform IME uses. It is never a global
/// document-wide table.
///
/// A platform IME (macOS `NSTextInputClient`, Android, iOS) addresses text in
/// UTF-16 code units, while the logical source and every [`TextOffset`] are
/// UTF-8 byte offsets. UTF-8 and UTF-16 lengths diverge non-linearly: an ASCII
/// scalar is one byte and one unit, a BMP scalar is two or three bytes but still
/// one unit, and an astral scalar (emoji, rare CJK) is four bytes and *two*
/// units (a surrogate pair). Converting per composition event by rescanning the
/// document — as reference implementations do — is O(n) every keystroke. Instead
/// this bridge builds a compact scalar-boundary table once for a composition or
/// paragraph slice and answers each direction with a binary search, then drops
/// the table when the composition ends. It is deliberately scoped: pass a
/// composition or paragraph substring, never the whole document.
///
/// The table records one `(utf8_byte, utf16_unit)` pair at the start of every
/// scalar, plus a trailing sentinel `(utf8_len, utf16_len)`. Both columns are
/// strictly increasing, so either coordinate locates the enclosing scalar by
/// binary search; within a scalar the two encodings advance in lockstep only for
/// ASCII, so interior queries snap to the scalar's start rather than inventing a
/// position inside it.
///
/// The source bytes are read verbatim via [`str::char_indices`] and
/// [`char::len_utf16`]; the bridge never NFC/NFKC-normalizes the text, so a
/// [`TextOffset`] recovered from a UTF-16 index always lands on the original,
/// unnormalized source (spec's no-implicit-normalize contract).
#[derive(Debug)]
pub struct Utf16Bridge {
    /// `(utf8_byte, utf16_unit)` at every scalar start, ascending in both
    /// columns, terminated by the `(utf8_len, utf16_len)` sentinel. An ordered
    /// `Vec` rather than a map: the columns are monotonic, so binary search is
    /// exact and the layout stays cache-friendly. `u32` suffices for a
    /// composition/paragraph-local slice. Never empty: an empty slice still
    /// carries the `(0, 0)` sentinel, so every query has a boundary to land on.
    scalar_starts: Vec<(u32, u32)>,
}

impl Default for Utf16Bridge {
    /// The empty bridge: a single `(0, 0)` sentinel, identical to
    /// [`Utf16Bridge::new`] on an empty slice. Both lengths are zero and every
    /// query maps `0 <-> 0`.
    fn default() -> Self {
        Self::new("")
    }
}

impl Utf16Bridge {
    /// Build the mapping table for one composition/paragraph-local `text`
    /// slice. This is O(n) in the slice length, run once per composition scope,
    /// never per event; the built table is dropped with the scope.
    pub fn new(text: &str) -> Self {
        let mut scalar_starts = Vec::new();
        let mut utf16_unit = 0u32;
        for (byte, ch) in text.char_indices() {
            scalar_starts.push((byte as u32, utf16_unit));
            utf16_unit += ch.len_utf16() as u32;
        }
        // Trailing sentinel so `text_len` is a queryable boundary and the last
        // scalar has a known extent.
        scalar_starts.push((text.len() as u32, utf16_unit));
        Self { scalar_starts }
    }

    /// The source slice length in UTF-8 bytes (the sentinel's byte column).
    pub fn utf8_len(&self) -> usize {
        self.scalar_starts
            .last()
            .map(|&(byte, _)| byte as usize)
            .unwrap_or(0)
    }

    /// The source slice length in UTF-16 code units (the sentinel's unit
    /// column).
    pub fn utf16_len(&self) -> usize {
        self.scalar_starts
            .last()
            .map(|&(_, unit)| unit as usize)
            .unwrap_or(0)
    }

    /// Map a UTF-16 code-unit index (as a platform IME reports it) to a UTF-8
    /// byte offset within this composition/paragraph scope.
    ///
    /// An index past the end clamps to the slice's byte length. An index that
    /// falls between the two units of a surrogate pair cannot name a scalar
    /// boundary, so it snaps to the start of that scalar rather than pointing
    /// into it — a caret never sits inside a scalar.
    pub fn utf16_to_offset(&self, utf16_unit: usize) -> TextOffset {
        let unit = utf16_unit as u32;
        match self.scalar_starts.binary_search_by_key(&unit, |&(_, u)| u) {
            // Exact scalar boundary (including the sentinel end).
            Ok(idx) => TextOffset(self.scalar_starts[idx].0 as usize),
            // Between two boundaries: `idx` is the first start strictly after
            // `unit`. Past the sentinel (`idx == len`) the index is out of
            // range and clamps to the byte length (the sentinel). Otherwise
            // `idx - 1` is the enclosing scalar, and an in-between unit
            // (surrogate middle) snaps to its start; `idx == 0` cannot happen
            // because the first start is unit 0.
            Err(idx) => {
                let target = idx.saturating_sub(1).min(self.scalar_starts.len() - 1);
                TextOffset(self.scalar_starts[target].0 as usize)
            }
        }
    }

    /// Map a UTF-8 byte offset within this scope to a UTF-16 code-unit index.
    ///
    /// `offset` is expected to sit on a scalar boundary. An interior byte (the
    /// middle of a multi-byte scalar) cannot name a UTF-16 boundary, so it snaps
    /// to the enclosing scalar's start; a byte past the end clamps to the
    /// slice's UTF-16 length.
    pub fn offset_to_utf16(&self, offset: TextOffset) -> usize {
        let byte = offset.0 as u32;
        match self.scalar_starts.binary_search_by_key(&byte, |&(b, _)| b) {
            Ok(idx) => self.scalar_starts[idx].1 as usize,
            // Past the sentinel clamps to the UTF-16 length; an interior byte
            // (mid-scalar) snaps to the enclosing scalar's unit start.
            Err(idx) => {
                let target = idx.saturating_sub(1).min(self.scalar_starts.len() - 1);
                self.scalar_starts[target].1 as usize
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A mixed sample spanning every UTF-8/UTF-16 width class: 'a' (1 byte,
    /// 1 unit), 'あ' (3 bytes, 1 unit, BMP), '𝄞' U+1D11E (4 bytes, 2 units,
    /// astral surrogate pair), 'b' (1 byte, 1 unit). The scalar starts are
    /// bytes 0/1/4/8 and units 0/1/2/4, ending at byte 9 / unit 5.
    const MIXED: &str = "aあ𝄞b";

    #[test]
    fn utf16_offset_roundtrip_is_identity() {
        // At every scalar boundary, mapping UTF-8 -> UTF-16 -> UTF-8 (and the
        // reverse) returns the same position: the two encodings agree on where
        // scalars begin.
        let bridge = Utf16Bridge::new(MIXED);
        for (byte, _) in MIXED
            .char_indices()
            .chain(std::iter::once((MIXED.len(), ' ')))
        {
            let offset = TextOffset(byte);
            let unit = bridge.offset_to_utf16(offset);
            assert_eq!(
                bridge.utf16_to_offset(unit),
                offset,
                "byte {byte} round-trip"
            );
        }
        // And forward from every valid UTF-16 boundary.
        for unit in [0usize, 1, 2, 4, 5] {
            let offset = bridge.utf16_to_offset(unit);
            assert_eq!(
                bridge.offset_to_utf16(offset),
                unit,
                "unit {unit} round-trip"
            );
        }
    }

    #[test]
    fn ascii_is_one_to_one() {
        // Pure ASCII: every byte offset equals its UTF-16 unit index.
        let bridge = Utf16Bridge::new("abcde");
        assert_eq!(bridge.utf8_len(), 5);
        assert_eq!(bridge.utf16_len(), 5);
        for i in 0..=5 {
            assert_eq!(bridge.offset_to_utf16(TextOffset(i)), i);
            assert_eq!(bridge.utf16_to_offset(i), TextOffset(i));
        }
    }

    #[test]
    fn bmp_char_maps_bytes_to_one_utf16_unit() {
        // 'あ' is 3 UTF-8 bytes but a single UTF-16 unit: byte offset 3 (after
        // the leading 'a') pairs with unit 1, and the trailing boundary is
        // byte 4 / unit 2.
        let bridge = Utf16Bridge::new("aあ");
        assert_eq!(bridge.utf8_len(), 4);
        assert_eq!(bridge.utf16_len(), 2);
        assert_eq!(bridge.offset_to_utf16(TextOffset(1)), 1);
        assert_eq!(bridge.utf16_to_offset(1), TextOffset(1));
        assert_eq!(bridge.offset_to_utf16(TextOffset(4)), 2);
        assert_eq!(bridge.utf16_to_offset(2), TextOffset(4));
    }

    #[test]
    fn astral_char_maps_to_surrogate_pair() {
        // A single astral emoji is 4 UTF-8 bytes and 2 UTF-16 units (a
        // surrogate pair): the trailing boundary is byte 4 / unit 2.
        let bridge = Utf16Bridge::new("\u{1F600}");
        assert_eq!(bridge.utf8_len(), 4);
        assert_eq!(bridge.utf16_len(), 2);
        assert_eq!(bridge.offset_to_utf16(TextOffset(0)), 0);
        assert_eq!(bridge.offset_to_utf16(TextOffset(4)), 2);
        assert_eq!(bridge.utf16_to_offset(2), TextOffset(4));
    }

    #[test]
    fn utf16_index_inside_surrogate_pair_snaps_to_scalar_start() {
        // In "aあ𝄞b" the astral '𝄞' spans units 2..4 (bytes 4..8). Unit 3 falls
        // between the pair's two units — it cannot name a scalar boundary, so it
        // snaps back to the scalar's start (byte 4), never into it.
        let bridge = Utf16Bridge::new(MIXED);
        assert_eq!(bridge.utf16_to_offset(3), TextOffset(4));
    }

    #[test]
    fn out_of_range_utf16_clamps_to_len() {
        // An index past the last unit clamps to the byte length.
        let bridge = Utf16Bridge::new(MIXED);
        assert_eq!(bridge.utf16_to_offset(5), TextOffset(9));
        assert_eq!(bridge.utf16_to_offset(99), TextOffset(9));
    }

    #[test]
    fn interior_byte_snaps_to_scalar_start() {
        // A byte inside 'あ' (byte 2, mid-scalar) snaps to that scalar's start
        // unit; a byte past the end clamps to the UTF-16 length.
        let bridge = Utf16Bridge::new("aあ");
        assert_eq!(bridge.offset_to_utf16(TextOffset(2)), 1);
        assert_eq!(bridge.offset_to_utf16(TextOffset(99)), 2);
    }

    #[test]
    fn bridge_does_not_normalize_source() {
        // "e\u{301}" (e + combining acute) is NFC-composable to "é" (U+00E9),
        // which would be 2 bytes / 1 unit. The bridge must keep the source as
        // given: 3 bytes / 2 units, with the combining mark at its own boundary
        // (byte 1 / unit 1). Round-tripping preserves the original byte offset,
        // proving no implicit normalization replaced the source.
        let source = "e\u{301}";
        assert_eq!(source.len(), 3, "sample is decomposed, not pre-composed");
        let bridge = Utf16Bridge::new(source);
        assert_eq!(bridge.utf8_len(), 3);
        assert_eq!(bridge.utf16_len(), 2);
        assert_eq!(bridge.offset_to_utf16(TextOffset(1)), 1);
        assert_eq!(bridge.utf16_to_offset(1), TextOffset(1));
        // The trailing boundary is the original 3-byte length, not NFC's 2.
        assert_eq!(bridge.utf16_to_offset(2), TextOffset(3));
    }

    #[test]
    fn empty_bridge_is_identity_zero() {
        for bridge in [Utf16Bridge::default(), Utf16Bridge::new("")] {
            assert_eq!(bridge.utf8_len(), 0);
            assert_eq!(bridge.utf16_len(), 0);
            assert_eq!(bridge.utf16_to_offset(0), TextOffset(0));
            assert_eq!(bridge.offset_to_utf16(TextOffset(0)), 0);
        }
    }

    #[test]
    fn text_offset_char_boundary_validation() {
        // "aあ": byte 0 ('a' start), 1 ('あ' start), 4 (end) are boundaries;
        // bytes 2 and 3 are inside 'あ' and are not.
        let text = "aあ";
        assert!(TextOffset(0).is_char_boundary(text));
        assert!(TextOffset(1).is_char_boundary(text));
        assert!(TextOffset(4).is_char_boundary(text));
        assert!(!TextOffset(2).is_char_boundary(text));
        assert!(!TextOffset(3).is_char_boundary(text));
    }

    #[test]
    fn text_position_affinity_constructors() {
        assert_eq!(
            TextPosition::upstream(TextOffset(3)),
            TextPosition {
                offset: TextOffset(3),
                affinity: CaretAffinity::Upstream,
            }
        );
        assert_eq!(
            TextPosition::downstream(TextOffset(3)),
            TextPosition {
                offset: TextOffset(3),
                affinity: CaretAffinity::Downstream,
            }
        );
    }
}
