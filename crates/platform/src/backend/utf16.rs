//! UTF-16 offsets, as the mobile text-input APIs count them, mapped onto
//! the UTF-8 strings the platform reports.

/// The byte offset of UTF-16 offset `units` in `text`, rounded down to a
/// character boundary (an offset between the halves of a surrogate pair
/// names the character they form).
pub(super) fn byte_offset(text: &str, units: usize) -> usize {
    let mut seen = 0;
    for (byte, c) in text.char_indices() {
        let next = seen + c.len_utf16();
        if next > units {
            return byte;
        }
        seen = next;
    }
    text.len()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn utf16_offsets_become_byte_offsets() {
        assert_eq!(byte_offset("a你😀b", 0), 0);
        assert_eq!(byte_offset("a你😀b", 1), 1);
        assert_eq!(byte_offset("a你😀b", 2), 4);
        // Between the surrogate halves of the emoji.
        assert_eq!(byte_offset("a你😀b", 3), 4);
        assert_eq!(byte_offset("a你😀b", 4), 8);
        assert_eq!(byte_offset("a你😀b", 99), 9);
    }
}
