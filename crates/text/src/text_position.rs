//! Typed logical text positions, the UTF-16 bridge, and caret affinity.
//!
//! A text position is never a bare `usize` shared across meanings. A byte
//! offset into UTF-8 source, a UTF-16 code-unit index a platform IME speaks in,
//! a grapheme boundary, a shaping cluster, and a glyph index are distinct types
//! with explicit conversions, so a position from one domain can never be
//! silently used in another. Source text is never implicitly normalized.

/// A byte offset into the UTF-8 logical source text.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct TextOffset(pub usize);

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

/// The composition-local / paragraph-local mapping between UTF-8 byte offsets
/// and the UTF-16 code-unit indices a platform IME uses. It is never a global
/// document-wide table.
#[derive(Debug, Default)]
pub struct Utf16Bridge {
    // TODO(TF-P2): composition/paragraph-local UTF-8 <-> UTF-16 mapping.
}

impl Utf16Bridge {
    /// Map a UTF-16 code-unit index (as a platform IME reports it) to a UTF-8
    /// byte offset within this composition/paragraph scope.
    pub fn utf16_to_offset(&self, _utf16_unit: usize) -> TextOffset {
        todo!("TF-P2: UTF-16 -> UTF-8 offset within composition scope")
    }
}
