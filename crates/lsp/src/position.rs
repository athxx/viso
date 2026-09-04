//! Byte ↔ UTF-16 position conversion (net-new piece 4).
//!
//! The language-server protocol addresses source by **UTF-16** line/character,
//! while the `viso-dsl` frontend speaks byte [`TextRange`]s. This module is the
//! thin two-way adapter between them, with its own minimal [`LspPosition`] /
//! [`LspRange`] data types so the engine never pulls in a protocol crate.
//!
//! The forward direction (byte → UTF-16) wraps
//! [`LineIndex::line_col_utf16`](viso_dsl::LineIndex::line_col_utf16), which
//! already has a pure-ASCII fast path. The reverse direction (UTF-16 → byte) is
//! implemented here against the source text: the frontend never needed it (it only
//! ever emits positions), but the server must map an incoming cursor position back
//! to a byte offset to answer goto/references/rename.
//!
//! All cold-path (AGENTS 7.2): a handful of these run per editor request.

use viso_dsl::LineIndex;
use viso_dsl::TextRange;
use viso_dsl::TextSize;

/// A zero-based UTF-16 position: line, then character offset within the line.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LspPosition {
    /// Zero-based line number.
    pub line: u32,
    /// Zero-based character offset within the line, in UTF-16 code units.
    pub character: u32,
}

/// A half-open range of two [`LspPosition`]s, as the protocol represents a span.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LspRange {
    /// The inclusive start position.
    pub start: LspPosition,
    /// The exclusive end position.
    pub end: LspPosition,
}

/// Converts a byte [`TextRange`] to a UTF-16 [`LspRange`] via the line index.
pub fn to_lsp_range(line_index: &LineIndex, range: TextRange) -> LspRange {
    LspRange {
        start: to_lsp_position(line_index, range.start()),
        end: to_lsp_position(line_index, range.end()),
    }
}

/// Converts a byte offset to a UTF-16 [`LspPosition`] via the line index.
pub fn to_lsp_position(line_index: &LineIndex, offset: TextSize) -> LspPosition {
    let lc = line_index.line_col_utf16(offset);
    LspPosition {
        line: lc.line,
        character: lc.column,
    }
}

/// Converts a UTF-16 [`LspPosition`] back to a byte offset in `source`.
///
/// Walks to the requested line, then accumulates UTF-16 code units across the
/// line's characters until `character` is reached, returning the byte offset at
/// that point. A `character` past the line's end clamps to the line end (the LSP
/// spec's recommended behavior for an out-of-range column), and a `line` past the
/// last line clamps to the source end — so a stale or slightly-off client position
/// never panics or indexes out of bounds.
pub fn from_lsp_position(source: &str, pos: LspPosition) -> TextSize {
    // Find the byte offset of the start of `pos.line`.
    let mut line = 0u32;
    let mut line_start = 0usize;
    if pos.line > 0 {
        for (i, b) in source.bytes().enumerate() {
            if b == b'\n' {
                line += 1;
                if line == pos.line {
                    line_start = i + 1;
                    break;
                }
            }
        }
        if line < pos.line {
            // The requested line is past the end of the source: clamp to the end.
            return TextSize::new(source.len() as u32);
        }
    }

    // Walk characters from the line start, counting UTF-16 code units, until we
    // reach `pos.character` or the end of the line (a `\n` or end of source).
    let mut utf16 = 0u32;
    let tail = &source[line_start..];
    for (byte_off, ch) in tail.char_indices() {
        if ch == '\n' {
            return TextSize::new((line_start + byte_off) as u32);
        }
        if utf16 >= pos.character {
            return TextSize::new((line_start + byte_off) as u32);
        }
        utf16 += ch.len_utf16() as u32;
    }
    // Reached end of source before `pos.character` (or exactly at the end).
    TextSize::new(source.len() as u32)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ascii_round_trips_through_utf16() {
        let src = "component Counter {\n  state count = 0;\n}\n";
        let li = LineIndex::new(src);
        // Offset of `count` on line 1.
        let byte = src.find("count").unwrap() as u32;
        let pos = to_lsp_position(&li, TextSize::new(byte));
        assert_eq!(pos.line, 1);
        // "  state " is 8 ASCII chars, so column 8.
        assert_eq!(pos.character, 8);
        assert_eq!(from_lsp_position(src, pos), TextSize::new(byte));
    }

    #[test]
    fn non_ascii_before_target_shifts_utf16_column() {
        // `π` is one UTF-16 unit but two UTF-8 bytes; `𝔸` is two UTF-16 units and
        // four UTF-8 bytes. A byte offset after them must convert to a smaller
        // UTF-16 column, and back to the same byte offset.
        let src = "let π = 𝔸x;";
        let li = LineIndex::new(src);
        let byte = src.find('x').unwrap() as u32;
        let pos = to_lsp_position(&li, TextSize::new(byte));
        // Chars before `x`: l e t ␣ π ␣ = ␣ 𝔸 → 8 BMP chars (1 unit each) plus 𝔸
        // (2 UTF-16 units) = 10 UTF-16 code units.
        assert_eq!(pos.line, 0);
        assert_eq!(pos.character, 10);
        assert_eq!(from_lsp_position(src, pos), TextSize::new(byte));
    }

    #[test]
    fn character_past_line_end_clamps_to_line_end() {
        let src = "ab\ncd\n";
        let pos = LspPosition {
            line: 0,
            character: 99,
        };
        // Clamps to the newline at offset 2 (end of line 0's content).
        assert_eq!(from_lsp_position(src, pos), TextSize::new(2));
    }

    #[test]
    fn line_past_end_clamps_to_source_end() {
        let src = "ab\n";
        let pos = LspPosition {
            line: 99,
            character: 0,
        };
        assert_eq!(from_lsp_position(src, pos), TextSize::new(src.len() as u32));
    }
}
