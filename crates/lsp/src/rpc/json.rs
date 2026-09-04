//! A minimal, self-contained JSON value with a parser and serializer.
//!
//! The language-server transport is JSON-RPC, but the workspace pulls in no
//! `serde`/`serde_json` dependency and the protocol surface this server implements
//! is small and fixed (a handful of request/response/notification shapes). A tiny
//! owned [`Json`] value with a recursive-descent parser and a string serializer is
//! the right cold-path adapter here (AGENTS 25 — a frontend adapts a protocol; it
//! does not pull in a general runtime), and it keeps the whole transport unit-
//! testable without a transport.
//!
//! This is not a general-purpose JSON library: it handles exactly the value forms
//! LSP messages use (objects, arrays, strings, numbers, booleans, null), decodes the
//! standard string escapes, and encodes strings with the escapes a JSON consumer
//! requires. Numbers are carried as `f64` (LSP integers all fit); `as_i64`/`as_u64`
//! read them back where an integer is expected.

use std::collections::BTreeMap;

/// A parsed or constructed JSON value.
#[derive(Debug, Clone, PartialEq)]
pub enum Json {
    /// JSON `null`.
    Null,
    /// A boolean.
    Bool(bool),
    /// A number, carried as `f64` (every LSP integer fits exactly).
    Num(f64),
    /// A string, unescaped.
    Str(String),
    /// An array of values.
    Arr(Vec<Json>),
    /// An object, key-ordered (deterministic serialization for golden tests).
    Obj(BTreeMap<String, Json>),
}

impl Json {
    /// The object member `key`, if this is an object that has it.
    pub fn get(&self, key: &str) -> Option<&Json> {
        match self {
            Json::Obj(m) => m.get(key),
            _ => None,
        }
    }

    /// This value as a string slice, if it is a string.
    pub fn as_str(&self) -> Option<&str> {
        match self {
            Json::Str(s) => Some(s),
            _ => None,
        }
    }

    /// This value as an array slice, if it is an array.
    pub fn as_arr(&self) -> Option<&[Json]> {
        match self {
            Json::Arr(a) => Some(a),
            _ => None,
        }
    }

    /// This value as an `i64`, if it is a number.
    pub fn as_i64(&self) -> Option<i64> {
        match self {
            Json::Num(n) => Some(*n as i64),
            _ => None,
        }
    }

    /// This value as a `u32`, if it is a non-negative number.
    pub fn as_u32(&self) -> Option<u32> {
        match self {
            Json::Num(n) if *n >= 0.0 => Some(*n as u32),
            _ => None,
        }
    }

    fn write(&self, out: &mut String) {
        match self {
            Json::Null => out.push_str("null"),
            Json::Bool(true) => out.push_str("true"),
            Json::Bool(false) => out.push_str("false"),
            Json::Num(n) => {
                // LSP numbers are integers; emit them without a fractional part when
                // they are whole, otherwise fall back to the default float format.
                if n.fract() == 0.0 && n.is_finite() {
                    out.push_str(&(*n as i64).to_string());
                } else {
                    out.push_str(&n.to_string());
                }
            }
            Json::Str(s) => write_json_string(s, out),
            Json::Arr(a) => {
                out.push('[');
                for (i, v) in a.iter().enumerate() {
                    if i > 0 {
                        out.push(',');
                    }
                    v.write(out);
                }
                out.push(']');
            }
            Json::Obj(m) => {
                out.push('{');
                for (i, (k, v)) in m.iter().enumerate() {
                    if i > 0 {
                        out.push(',');
                    }
                    write_json_string(k, out);
                    out.push(':');
                    v.write(out);
                }
                out.push('}');
            }
        }
    }
}

impl std::fmt::Display for Json {
    /// Serializes to a compact JSON string.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let mut out = String::new();
        self.write(&mut out);
        f.write_str(&out)
    }
}

/// Writes `s` as a quoted, escaped JSON string.
fn write_json_string(s: &str, out: &mut String) {
    out.push('"');
    for ch in s.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => {
                out.push_str(&format!("\\u{:04x}", c as u32));
            }
            c => out.push(c),
        }
    }
    out.push('"');
}

/// Parses a JSON document, returning the value or `None` on any malformed input.
pub fn parse(input: &str) -> Option<Json> {
    let mut p = Parser {
        bytes: input.as_bytes(),
        pos: 0,
    };
    p.skip_ws();
    let v = p.value()?;
    p.skip_ws();
    // Trailing non-whitespace is a malformed document.
    if p.pos == p.bytes.len() {
        Some(v)
    } else {
        None
    }
}

/// A single-pass recursive-descent JSON parser over a byte slice.
struct Parser<'a> {
    bytes: &'a [u8],
    pos: usize,
}

impl Parser<'_> {
    fn peek(&self) -> Option<u8> {
        self.bytes.get(self.pos).copied()
    }

    fn bump(&mut self) -> Option<u8> {
        let b = self.peek()?;
        self.pos += 1;
        Some(b)
    }

    fn skip_ws(&mut self) {
        while matches!(self.peek(), Some(b' ' | b'\t' | b'\n' | b'\r')) {
            self.pos += 1;
        }
    }

    fn value(&mut self) -> Option<Json> {
        self.skip_ws();
        match self.peek()? {
            b'{' => self.object(),
            b'[' => self.array(),
            b'"' => self.string().map(Json::Str),
            b't' | b'f' => self.boolean(),
            b'n' => self.null(),
            b'-' | b'0'..=b'9' => self.number(),
            _ => None,
        }
    }

    fn object(&mut self) -> Option<Json> {
        self.bump(); // '{'
        let mut map = BTreeMap::new();
        self.skip_ws();
        if self.peek() == Some(b'}') {
            self.bump();
            return Some(Json::Obj(map));
        }
        loop {
            self.skip_ws();
            let key = self.string()?;
            self.skip_ws();
            if self.bump()? != b':' {
                return None;
            }
            let val = self.value()?;
            map.insert(key, val);
            self.skip_ws();
            match self.bump()? {
                b',' => continue,
                b'}' => return Some(Json::Obj(map)),
                _ => return None,
            }
        }
    }

    fn array(&mut self) -> Option<Json> {
        self.bump(); // '['
        let mut arr = Vec::new();
        self.skip_ws();
        if self.peek() == Some(b']') {
            self.bump();
            return Some(Json::Arr(arr));
        }
        loop {
            let val = self.value()?;
            arr.push(val);
            self.skip_ws();
            match self.bump()? {
                b',' => continue,
                b']' => return Some(Json::Arr(arr)),
                _ => return None,
            }
        }
    }

    fn string(&mut self) -> Option<String> {
        if self.bump()? != b'"' {
            return None;
        }
        let mut s = String::new();
        loop {
            match self.bump()? {
                b'"' => return Some(s),
                b'\\' => match self.bump()? {
                    b'"' => s.push('"'),
                    b'\\' => s.push('\\'),
                    b'/' => s.push('/'),
                    b'n' => s.push('\n'),
                    b'r' => s.push('\r'),
                    b't' => s.push('\t'),
                    b'b' => s.push('\u{0008}'),
                    b'f' => s.push('\u{000c}'),
                    b'u' => {
                        let cp = self.hex4()?;
                        // A high surrogate must be followed by a `\uXXXX` low surrogate.
                        if (0xD800..=0xDBFF).contains(&cp) {
                            if self.bump()? != b'\\' || self.bump()? != b'u' {
                                return None;
                            }
                            let lo = self.hex4()?;
                            if !(0xDC00..=0xDFFF).contains(&lo) {
                                return None;
                            }
                            let c = 0x10000 + ((cp - 0xD800) << 10) + (lo - 0xDC00);
                            s.push(char::from_u32(c)?);
                        } else {
                            s.push(char::from_u32(cp)?);
                        }
                    }
                    _ => return None,
                },
                // A raw control byte is invalid inside a JSON string.
                b if b < 0x20 => return None,
                b => {
                    // Re-decode the (possibly multi-byte UTF-8) character starting here.
                    self.pos -= 1;
                    let start = self.pos;
                    let len = utf8_len(b);
                    let slice = self.bytes.get(start..start + len)?;
                    let text = std::str::from_utf8(slice).ok()?;
                    s.push_str(text);
                    self.pos += len;
                }
            }
        }
    }

    fn hex4(&mut self) -> Option<u32> {
        let mut v = 0u32;
        for _ in 0..4 {
            let b = self.bump()?;
            let d = (b as char).to_digit(16)?;
            v = v * 16 + d;
        }
        Some(v)
    }

    fn boolean(&mut self) -> Option<Json> {
        if self.bytes[self.pos..].starts_with(b"true") {
            self.pos += 4;
            Some(Json::Bool(true))
        } else if self.bytes[self.pos..].starts_with(b"false") {
            self.pos += 5;
            Some(Json::Bool(false))
        } else {
            None
        }
    }

    fn null(&mut self) -> Option<Json> {
        if self.bytes[self.pos..].starts_with(b"null") {
            self.pos += 4;
            Some(Json::Null)
        } else {
            None
        }
    }

    fn number(&mut self) -> Option<Json> {
        let start = self.pos;
        if self.peek() == Some(b'-') {
            self.pos += 1;
        }
        while matches!(self.peek(), Some(b'0'..=b'9')) {
            self.pos += 1;
        }
        if self.peek() == Some(b'.') {
            self.pos += 1;
            while matches!(self.peek(), Some(b'0'..=b'9')) {
                self.pos += 1;
            }
        }
        if matches!(self.peek(), Some(b'e' | b'E')) {
            self.pos += 1;
            if matches!(self.peek(), Some(b'+' | b'-')) {
                self.pos += 1;
            }
            while matches!(self.peek(), Some(b'0'..=b'9')) {
                self.pos += 1;
            }
        }
        let text = std::str::from_utf8(&self.bytes[start..self.pos]).ok()?;
        text.parse::<f64>().ok().map(Json::Num)
    }
}

/// The UTF-8 encoded length of the character whose leading byte is `b`.
fn utf8_len(b: u8) -> usize {
    if b < 0x80 {
        1
    } else if b >> 5 == 0b110 {
        2
    } else if b >> 4 == 0b1110 {
        3
    } else {
        4
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_a_request_shape() {
        let src = r#"{"jsonrpc":"2.0","id":1,"method":"textDocument/definition","params":{"position":{"line":2,"character":15}}}"#;
        let v = parse(src).expect("parses");
        assert_eq!(
            v.get("method").and_then(Json::as_str),
            Some("textDocument/definition")
        );
        assert_eq!(
            v.get("params")
                .and_then(|p| p.get("position"))
                .and_then(|p| p.get("line"))
                .and_then(Json::as_u32),
            Some(2)
        );
        // Re-serializing and re-parsing yields the same value (BTreeMap key order is
        // stable, so this is a true round trip).
        assert_eq!(parse(&v.to_string()).unwrap(), v);
    }

    #[test]
    fn decodes_and_encodes_escapes() {
        let v = parse(r#""a\"b\\c\ndé""#).expect("parses");
        assert_eq!(v.as_str(), Some("a\"b\\c\nd\u{e9}"));
        // The newline and quote must be re-escaped on the way out.
        let s = v.to_string();
        assert!(s.contains("\\n"));
        assert!(s.contains("\\\""));
    }

    #[test]
    fn rejects_malformed_input() {
        assert!(parse("{").is_none());
        assert!(parse("").is_none());
        assert!(parse("[1,2").is_none());
        assert!(parse("nul").is_none());
        assert!(parse(r#"{"a":1} trailing"#).is_none());
    }

    #[test]
    fn surrogate_pair_decodes_to_astral_char() {
        // U+1D538 (𝔸) is the surrogate pair D835 DD38.
        let v = parse(r#""𝔸""#).expect("parses");
        assert_eq!(v.as_str(), Some("𝔸"));
    }
}
