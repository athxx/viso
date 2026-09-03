//! Source positions: a byte-primary [`TextSize`] / [`TextRange`], plus a
//! [`LineIndex`] that derives the other coordinate systems on demand.
//!
//! Every token stores exactly one span, in **UTF-8 byte offsets** — the natural
//! coordinate for `&source[range]` slicing, `Copy`, and four bytes each. Editors
//! and the language server also need line/column, Unicode-scalar, and UTF-16
//! coordinates (spec section 9), but storing three ranges on every token would triple
//! the hot token vector for data that is only queried at the margins. Instead a
//! [`LineIndex`] is built once per source and converts a byte offset into any of
//! those coordinates in `O(log lines)`; tokens stay lean.

use std::ops::{Add, Range, Sub};

/// A size or absolute offset into a source file, in UTF-8 bytes.
///
/// A newtype over `u32`: a source file is capped at 4 GiB, which no `.vs` file
/// approaches, and `u32` keeps [`TextRange`] eight bytes and `Copy`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
pub struct TextSize(u32);

impl TextSize {
    /// The zero offset / empty size.
    pub const ZERO: TextSize = TextSize(0);

    /// Wraps a raw byte count. Sizes above `u32::MAX` cannot occur for a valid
    /// source (the lexer rejects oversized input before building spans).
    #[inline]
    pub const fn new(bytes: u32) -> TextSize {
        TextSize(bytes)
    }

    /// The raw byte offset.
    #[inline]
    pub const fn to_u32(self) -> u32 {
        self.0
    }

    /// The raw byte offset as a `usize`, for slicing.
    #[inline]
    pub const fn to_usize(self) -> usize {
        self.0 as usize
    }
}

impl From<u32> for TextSize {
    #[inline]
    fn from(v: u32) -> Self {
        TextSize(v)
    }
}

impl TryFrom<usize> for TextSize {
    type Error = std::num::TryFromIntError;
    #[inline]
    fn try_from(v: usize) -> Result<Self, Self::Error> {
        Ok(TextSize(u32::try_from(v)?))
    }
}

impl Add for TextSize {
    type Output = TextSize;
    #[inline]
    fn add(self, rhs: TextSize) -> TextSize {
        TextSize(self.0 + rhs.0)
    }
}

impl Sub for TextSize {
    type Output = TextSize;
    #[inline]
    fn sub(self, rhs: TextSize) -> TextSize {
        TextSize(self.0 - rhs.0)
    }
}

/// A half-open `[start, end)` byte range in a source file.
///
/// The primary span type on every token and green node. Half-open so adjacent
/// ranges share an endpoint and lengths are `end - start`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct TextRange {
    start: TextSize,
    end: TextSize,
}

impl TextRange {
    /// A range from `start` to `end`. `start <= end` must hold; callers in the
    /// lexer always advance the cursor forward, so this is upheld by
    /// construction.
    #[inline]
    pub const fn new(start: TextSize, end: TextSize) -> TextRange {
        debug_assert!(start.0 <= end.0, "TextRange start must not exceed end");
        TextRange { start, end }
    }

    /// An empty range at `offset` — used for a [`super::SyntaxKind::MissingToken`]
    /// the parser inserts where the source omitted a required token.
    #[inline]
    pub const fn empty(offset: TextSize) -> TextRange {
        TextRange {
            start: offset,
            end: offset,
        }
    }

    /// The start offset.
    #[inline]
    pub const fn start(self) -> TextSize {
        self.start
    }

    /// The end offset (exclusive).
    #[inline]
    pub const fn end(self) -> TextSize {
        self.end
    }

    /// The length in bytes.
    #[inline]
    pub const fn len(self) -> TextSize {
        TextSize(self.end.0 - self.start.0)
    }

    /// Whether the range is empty (zero bytes).
    #[inline]
    pub const fn is_empty(self) -> bool {
        self.start.0 == self.end.0
    }

    /// As a `usize` [`Range`] for slicing a `&str`/`&[u8]`.
    #[inline]
    pub const fn as_usize(self) -> Range<usize> {
        self.start.to_usize()..self.end.to_usize()
    }

    /// Whether `offset` lies within `[start, end)`.
    #[inline]
    pub const fn contains(self, offset: TextSize) -> bool {
        self.start.0 <= offset.0 && offset.0 < self.end.0
    }
}

impl From<TextRange> for Range<usize> {
    #[inline]
    fn from(r: TextRange) -> Range<usize> {
        r.as_usize()
    }
}

/// A one-based line, zero-based column position in a chosen coordinate system.
///
/// `column` is measured in the units named by whichever [`LineIndex`] method
/// produced it (bytes, Unicode scalars, or UTF-16 code units). Line and column
/// are what a human and most editor protocols expect; the language-server
/// protocol specifically wants UTF-16 columns.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LineCol {
    /// Zero-based line number.
    pub line: u32,
    /// Zero-based column, in the coordinate unit of the producing call.
    pub column: u32,
}

/// A per-source map from byte offset to line/column in three coordinate systems.
///
/// Built once from the full source (a scan recording each line's start offset,
/// plus every multi-byte UTF-8 sequence so scalar and UTF-16 columns can be
/// recovered without rescanning). Tokens keep only their byte [`TextRange`]; a
/// caller that needs line/column or UTF-16 positions consults this map.
#[derive(Debug, Clone)]
pub struct LineIndex {
    /// Byte offset of the first character of each line. `line_starts[0] == 0`.
    line_starts: Vec<TextSize>,
    /// Every non-ASCII UTF-8 sequence in the source, ascending by offset, so a
    /// byte offset can be converted to a scalar or UTF-16 column by counting the
    /// multi-byte sequences before it on its line. Empty for pure-ASCII sources
    /// (the common case), making the conversions a plain byte subtraction.
    wide_chars: Vec<WideChar>,
}

/// A single non-ASCII character, recorded for coordinate conversion.
#[derive(Debug, Clone, Copy)]
struct WideChar {
    /// Byte offset of the character's first byte.
    offset: TextSize,
    /// The character's length in UTF-8 bytes (2..=4).
    len_utf8: u8,
    /// The character's length in UTF-16 code units (1 for the BMP, 2 above it).
    len_utf16: u8,
}

impl LineIndex {
    /// Builds the index by scanning `source` once.
    pub fn new(source: &str) -> LineIndex {
        let mut line_starts = vec![TextSize::ZERO];
        let mut wide_chars = Vec::new();

        for (offset, ch) in source.char_indices() {
            let offset = TextSize::new(offset as u32);
            if ch == '\n' {
                line_starts.push(TextSize::new(offset.to_u32() + 1));
            }
            let len_utf8 = ch.len_utf8();
            if len_utf8 > 1 {
                wide_chars.push(WideChar {
                    offset,
                    len_utf8: len_utf8 as u8,
                    len_utf16: ch.len_utf16() as u8,
                });
            }
        }

        LineIndex {
            line_starts,
            wide_chars,
        }
    }

    /// The number of lines in the source (at least one).
    #[inline]
    pub fn line_count(&self) -> u32 {
        self.line_starts.len() as u32
    }

    /// The zero-based line containing `offset`, and that line's start offset.
    fn line_of(&self, offset: TextSize) -> (u32, TextSize) {
        // `partition_point` finds the first line start strictly greater than
        // `offset`; the line before it is the one containing `offset`.
        let line = self
            .line_starts
            .partition_point(|&start| start <= offset)
            .saturating_sub(1);
        (line as u32, self.line_starts[line])
    }

    /// The line/column of `offset`, with the column counted in **UTF-8 bytes**.
    pub fn line_col_utf8(&self, offset: TextSize) -> LineCol {
        let (line, line_start) = self.line_of(offset);
        LineCol {
            line,
            column: offset.to_u32() - line_start.to_u32(),
        }
    }

    /// The line/column of `offset`, with the column counted in **Unicode
    /// scalar values** (`char`s) — one column per `char`, regardless of its
    /// byte width.
    pub fn line_col_scalar(&self, offset: TextSize) -> LineCol {
        let (line, line_start) = self.line_of(offset);
        let byte_col = offset.to_u32() - line_start.to_u32();
        // Each wide char on this line before `offset` occupies `len_utf8` bytes
        // but a single scalar column, so subtract its extra bytes.
        let mut column = byte_col;
        for wc in self.wide_on_line(line_start, offset) {
            column -= u32::from(wc.len_utf8 - 1);
        }
        LineCol { line, column }
    }

    /// The line/column of `offset`, with the column counted in **UTF-16 code
    /// units** — the coordinate the language-server protocol uses.
    pub fn line_col_utf16(&self, offset: TextSize) -> LineCol {
        let (line, line_start) = self.line_of(offset);
        let byte_col = offset.to_u32() - line_start.to_u32();
        // Each wide char contributes `len_utf16` units instead of `len_utf8`
        // bytes; adjust the byte column by that per-char delta.
        let mut column = byte_col;
        for wc in self.wide_on_line(line_start, offset) {
            column -= u32::from(wc.len_utf8) - u32::from(wc.len_utf16);
        }
        LineCol { line, column }
    }

    /// The wide chars in `[line_start, offset)`, for a per-line column
    /// adjustment. `wide_chars` is offset-sorted, so this is a bounded slice
    /// scan; for the common pure-ASCII source it is empty.
    fn wide_on_line(
        &self,
        line_start: TextSize,
        offset: TextSize,
    ) -> impl Iterator<Item = &WideChar> {
        let lo = self.wide_chars.partition_point(|wc| wc.offset < line_start);
        self.wide_chars[lo..]
            .iter()
            .take_while(move |wc| wc.offset < offset)
    }
}
