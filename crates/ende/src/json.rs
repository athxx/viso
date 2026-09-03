//! A minimal JSON emitter for diagnostics and tool interchange.
//!
//! Write-side only, hand-rolled, no serde: it exists so diagnostics and tool
//! output have one canonical JSON shape without pulling a serialization
//! framework into a leaf crate. The target consumer is the diagnostic schema —
//! objects, arrays, strings, numbers, bools, null — so that is exactly what this
//! emits, with correct string escaping and no more.
//!
//! [`JsonWriter`] is a thin state machine over an owned `String`. It inserts
//! structural commas for you (so callers never manage separators) and escapes
//! strings per the JSON spec. It intentionally does not pretty-print: tool
//! interchange wants compact, stable output.

/// A compact JSON writer.
///
/// Drive it with the structural methods (`begin_object` / `end_object`,
/// `begin_array` / `end_array`, `name`) and the value methods (`string`,
/// `number`, `bool`, `null`). It tracks whether the next value needs a leading
/// comma, so callers never write separators by hand. Misuse (a value where a
/// name is due, or vice versa) is a caller bug, not a runtime check — this is a
/// tooling emitter, not a validator.
#[derive(Debug, Default, Clone)]
pub struct JsonWriter {
    out: String,
    /// True when the next token at the current nesting must be preceded by a
    /// comma (i.e. at least one element/member has already been written here).
    needs_comma: bool,
}

impl JsonWriter {
    /// A new, empty writer.
    #[inline]
    pub fn new() -> Self {
        Self {
            out: String::new(),
            needs_comma: false,
        }
    }

    /// The JSON written so far.
    #[inline]
    pub fn as_str(&self) -> &str {
        &self.out
    }

    /// Consumes the writer and returns the JSON string.
    #[inline]
    pub fn into_string(self) -> String {
        self.out
    }

    /// Writes a structural comma if a sibling already exists at this level, then
    /// clears the flag; call before every value or member name.
    #[inline]
    fn separator(&mut self) {
        if self.needs_comma {
            self.out.push(',');
        }
        self.needs_comma = false;
    }

    /// Opens an object `{`. Pair with [`end_object`](Self::end_object).
    #[inline]
    pub fn begin_object(&mut self) {
        self.separator();
        self.out.push('{');
    }

    /// Closes an object `}`.
    #[inline]
    pub fn end_object(&mut self) {
        self.out.push('}');
        // A completed object is itself a sibling for whatever follows it.
        self.needs_comma = true;
    }

    /// Opens an array `[`. Pair with [`end_array`](Self::end_array).
    #[inline]
    pub fn begin_array(&mut self) {
        self.separator();
        self.out.push('[');
    }

    /// Closes an array `]`.
    #[inline]
    pub fn end_array(&mut self) {
        self.out.push(']');
        self.needs_comma = true;
    }

    /// Writes an object member name and its `:`. The next call writes the value.
    #[inline]
    pub fn name(&mut self, key: &str) {
        self.separator();
        write_escaped(&mut self.out, key);
        self.out.push(':');
        // The name and its value are one member; the value must not be
        // comma-separated from the name, so leave the flag clear.
        self.needs_comma = false;
    }

    /// Writes a JSON string value (escaped).
    #[inline]
    pub fn string(&mut self, value: &str) {
        self.separator();
        write_escaped(&mut self.out, value);
        self.needs_comma = true;
    }

    /// Writes a boolean value.
    #[inline]
    pub fn bool(&mut self, value: bool) {
        self.separator();
        self.out.push_str(if value { "true" } else { "false" });
        self.needs_comma = true;
    }

    /// Writes a JSON null.
    #[inline]
    pub fn null(&mut self) {
        self.separator();
        self.out.push_str("null");
        self.needs_comma = true;
    }

    /// Writes a signed integer value.
    #[inline]
    pub fn int(&mut self, value: i64) {
        self.separator();
        let mut buf = itoa_buf();
        self.out.push_str(buf.format_i64(value));
        self.needs_comma = true;
    }

    /// Writes an unsigned integer value.
    #[inline]
    pub fn uint(&mut self, value: u64) {
        self.separator();
        let mut buf = itoa_buf();
        self.out.push_str(buf.format_u64(value));
        self.needs_comma = true;
    }

    /// Writes a floating-point value.
    ///
    /// JSON has no representation for a non-finite number, so `NaN` and the
    /// infinities are emitted as `null` (the conventional lossy choice for
    /// diagnostics output).
    #[inline]
    pub fn number(&mut self, value: f64) {
        self.separator();
        if value.is_finite() {
            // `{}` on f64 gives a round-trippable, JSON-compatible decimal for
            // finite values (no exponent-only forms that JSON would reject).
            use core::fmt::Write as _;
            let _ = write!(self.out, "{value}");
        } else {
            self.out.push_str("null");
        }
        self.needs_comma = true;
    }
}

/// Writes `value` as a quoted, escaped JSON string into `out`.
///
/// Escapes the JSON-mandatory set — `"`, `\`, and the C0 control characters —
/// using the short forms where they exist (`\n`, `\r`, `\t`, `\b`, `\f`) and
/// `\u00XX` otherwise. All other characters, including non-ASCII UTF-8, pass
/// through verbatim (valid in JSON).
fn write_escaped(out: &mut String, value: &str) {
    out.push('"');
    for ch in value.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '\u{08}' => out.push_str("\\b"),
            '\u{0c}' => out.push_str("\\f"),
            c if (c as u32) < 0x20 => {
                const HEX: &[u8; 16] = b"0123456789abcdef";
                let code = c as u32;
                out.push_str("\\u00");
                out.push(HEX[((code >> 4) & 0xf) as usize] as char);
                out.push(HEX[(code & 0xf) as usize] as char);
            }
            c => out.push(c),
        }
    }
    out.push('"');
}

/// A tiny stack integer formatter so the JSON writer needs no allocation per
/// number and no third-party dependency. `u64::MAX` and `i64::MIN` both fit in
/// 20 digits plus a sign.
struct IntBuf {
    bytes: [u8; 20],
}

#[inline]
fn itoa_buf() -> IntBuf {
    IntBuf { bytes: [0; 20] }
}

impl IntBuf {
    /// Formats an unsigned integer, returning a borrowed decimal string.
    fn format_u64(&mut self, mut value: u64) -> &str {
        let mut i = self.bytes.len();
        // Emit digits least-significant first from the tail of the buffer.
        loop {
            i -= 1;
            self.bytes[i] = b'0' + (value % 10) as u8;
            value /= 10;
            if value == 0 {
                break;
            }
        }
        // SAFETY: the bytes written are ASCII digits, hence valid UTF-8.
        core::str::from_utf8(&self.bytes[i..]).unwrap_or("0")
    }

    /// Formats a signed integer via its unsigned magnitude, prefixing `-`.
    fn format_i64(&mut self, value: i64) -> &str {
        if value >= 0 {
            return self.format_u64(value as u64);
        }
        // Negate through u64 so i64::MIN does not overflow.
        let magnitude = (value as i128).unsigned_abs() as u64;
        let mut i = self.bytes.len();
        let mut v = magnitude;
        loop {
            i -= 1;
            self.bytes[i] = b'0' + (v % 10) as u8;
            v /= 10;
            if v == 0 {
                break;
            }
        }
        i -= 1;
        self.bytes[i] = b'-';
        core::str::from_utf8(&self.bytes[i..]).unwrap_or("0")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn object_with_members_is_compact_and_comma_separated() {
        let mut w = JsonWriter::new();
        w.begin_object();
        w.name("severity");
        w.string("error");
        w.name("code");
        w.uint(42);
        w.name("ok");
        w.bool(false);
        w.name("note");
        w.null();
        w.end_object();
        assert_eq!(
            w.as_str(),
            r#"{"severity":"error","code":42,"ok":false,"note":null}"#
        );
    }

    #[test]
    fn nested_array_of_objects() {
        let mut w = JsonWriter::new();
        w.begin_object();
        w.name("spans");
        w.begin_array();
        w.begin_object();
        w.name("start");
        w.uint(0);
        w.name("end");
        w.uint(5);
        w.end_object();
        w.begin_object();
        w.name("start");
        w.uint(7);
        w.name("end");
        w.uint(9);
        w.end_object();
        w.end_array();
        w.end_object();
        assert_eq!(
            w.as_str(),
            r#"{"spans":[{"start":0,"end":5},{"start":7,"end":9}]}"#
        );
    }

    #[test]
    fn strings_are_escaped() {
        let mut w = JsonWriter::new();
        // Contains a quote, backslash, newline, tab, and a C0 control char.
        w.string("a\"b\\c\nd\te\u{01}f");
        assert_eq!(w.as_str(), r#""a\"b\\c\nd\te\u0001f""#);
    }

    #[test]
    fn unicode_passes_through_verbatim() {
        let mut w = JsonWriter::new();
        w.string("世界 → ✓");
        assert_eq!(w.as_str(), "\"世界 → ✓\"");
    }

    #[test]
    fn numbers_cover_signs_extremes_and_non_finite() {
        let mut w = JsonWriter::new();
        w.begin_array();
        w.int(i64::MIN);
        w.int(-1);
        w.int(0);
        w.uint(u64::MAX);
        w.number(1.5);
        w.number(f64::NAN);
        w.number(f64::INFINITY);
        w.end_array();
        assert_eq!(
            w.as_str(),
            "[-9223372036854775808,-1,0,18446744073709551615,1.5,null,null]"
        );
    }
}
