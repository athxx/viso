//! A bounded JSON reader for tool interchange: the read side of
//! [`JsonWriter`](crate::JsonWriter), for a tool reading another tool's JSON
//! (Cargo's metadata and build messages).
//!
//! [`JsonValue::parse`] reads one RFC 8259 text into a value tree, strictly:
//! no comments, no trailing commas, no leading zeros, every escape checked and
//! every surrogate paired. Nesting is bounded, so hostile input cannot
//! overflow the stack, and an object keeps its members in order, duplicates
//! included, for the caller to look up.

use std::fmt;

/// How deep arrays and objects may nest.
const MAX_DEPTH: usize = 128;

/// A JSON value.
#[derive(Debug, Clone, PartialEq)]
pub enum JsonValue {
    Null,
    Bool(bool),
    Number(f64),
    String(String),
    Array(Vec<JsonValue>),
    /// Members in document order.
    Object(Vec<(String, JsonValue)>),
}

/// Why a text is not JSON: what was wrong and at which byte.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JsonError {
    pub offset: usize,
    pub message: &'static str,
}

impl fmt::Display for JsonError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} at byte {}", self.message, self.offset)
    }
}

impl std::error::Error for JsonError {}

impl JsonValue {
    /// The value `text` holds, whitespace around it allowed.
    ///
    /// # Errors
    ///
    /// Text that is not one JSON value, or nests deeper than 128.
    pub fn parse(text: &str) -> Result<JsonValue, JsonError> {
        let mut reader = Reader {
            bytes: text.as_bytes(),
            at: 0,
        };
        reader.space();
        let value = reader.value(0)?;
        reader.space();
        if reader.at != reader.bytes.len() {
            return Err(reader.error("text after the value"));
        }
        Ok(value)
    }

    /// The first member `key` of an object.
    pub fn get(&self, key: &str) -> Option<&JsonValue> {
        match self {
            JsonValue::Object(members) => members.iter().find(|(k, _)| k == key).map(|(_, v)| v),
            _ => None,
        }
    }

    pub fn as_str(&self) -> Option<&str> {
        match self {
            JsonValue::String(s) => Some(s),
            _ => None,
        }
    }

    pub fn as_array(&self) -> Option<&[JsonValue]> {
        match self {
            JsonValue::Array(items) => Some(items),
            _ => None,
        }
    }

    pub fn as_bool(&self) -> Option<bool> {
        match self {
            JsonValue::Bool(b) => Some(*b),
            _ => None,
        }
    }

    pub fn as_f64(&self) -> Option<f64> {
        match self {
            JsonValue::Number(n) => Some(*n),
            _ => None,
        }
    }

    pub fn is_null(&self) -> bool {
        matches!(self, JsonValue::Null)
    }
}

struct Reader<'a> {
    bytes: &'a [u8],
    at: usize,
}

impl Reader<'_> {
    fn error(&self, message: &'static str) -> JsonError {
        JsonError {
            offset: self.at,
            message,
        }
    }

    fn peek(&self) -> Option<u8> {
        self.bytes.get(self.at).copied()
    }

    fn space(&mut self) {
        while matches!(self.peek(), Some(b' ' | b'\t' | b'\n' | b'\r')) {
            self.at += 1;
        }
    }

    fn eat(&mut self, byte: u8) -> bool {
        let found = self.peek() == Some(byte);
        self.at += usize::from(found);
        found
    }

    fn literal(&mut self, word: &'static str, value: JsonValue) -> Result<JsonValue, JsonError> {
        if self.bytes[self.at..].starts_with(word.as_bytes()) {
            self.at += word.len();
            Ok(value)
        } else {
            Err(self.error("not a value"))
        }
    }

    fn value(&mut self, depth: usize) -> Result<JsonValue, JsonError> {
        match self.peek() {
            Some(b'n') => self.literal("null", JsonValue::Null),
            Some(b't') => self.literal("true", JsonValue::Bool(true)),
            Some(b'f') => self.literal("false", JsonValue::Bool(false)),
            Some(b'"') => self.string().map(JsonValue::String),
            Some(b'[' | b'{') if depth == MAX_DEPTH => Err(self.error("nested too deep")),
            Some(b'[') => self.array(depth + 1),
            Some(b'{') => self.object(depth + 1),
            Some(b'-' | b'0'..=b'9') => self.number(),
            Some(_) => Err(self.error("not a value")),
            None => Err(self.error("the text ends before a value")),
        }
    }

    fn array(&mut self, depth: usize) -> Result<JsonValue, JsonError> {
        self.at += 1;
        let mut items = Vec::new();
        self.space();
        if self.eat(b']') {
            return Ok(JsonValue::Array(items));
        }
        loop {
            self.space();
            items.push(self.value(depth)?);
            self.space();
            if self.eat(b']') {
                return Ok(JsonValue::Array(items));
            }
            if !self.eat(b',') {
                return Err(self.error("expected `,` or `]`"));
            }
        }
    }

    fn object(&mut self, depth: usize) -> Result<JsonValue, JsonError> {
        self.at += 1;
        let mut members = Vec::new();
        self.space();
        if self.eat(b'}') {
            return Ok(JsonValue::Object(members));
        }
        loop {
            self.space();
            if self.peek() != Some(b'"') {
                return Err(self.error("expected a member name"));
            }
            let key = self.string()?;
            self.space();
            if !self.eat(b':') {
                return Err(self.error("expected `:`"));
            }
            self.space();
            members.push((key, self.value(depth)?));
            self.space();
            if self.eat(b'}') {
                return Ok(JsonValue::Object(members));
            }
            if !self.eat(b',') {
                return Err(self.error("expected `,` or `}`"));
            }
        }
    }

    fn number(&mut self) -> Result<JsonValue, JsonError> {
        let start = self.at;
        self.eat(b'-');
        let digits = |r: &mut Self| {
            let from = r.at;
            while matches!(r.peek(), Some(b'0'..=b'9')) {
                r.at += 1;
            }
            r.at - from
        };
        match self.peek() {
            Some(b'0') => self.at += 1,
            Some(b'1'..=b'9') => {
                digits(self);
            }
            _ => return Err(self.error("a number needs digits")),
        }
        if self.eat(b'.') && digits(self) == 0 {
            return Err(self.error("a fraction needs digits"));
        }
        if matches!(self.peek(), Some(b'e' | b'E')) {
            self.at += 1;
            if !self.eat(b'+') {
                self.eat(b'-');
            }
            if digits(self) == 0 {
                return Err(self.error("an exponent needs digits"));
            }
        }
        // ASCII by construction.
        let text = std::str::from_utf8(&self.bytes[start..self.at]).expect("ASCII digits");
        text.parse()
            .map(JsonValue::Number)
            .map_err(|_| self.error("not a number"))
    }

    fn string(&mut self) -> Result<String, JsonError> {
        self.at += 1;
        let mut out = String::new();
        loop {
            let run = self.at;
            while matches!(self.peek(), Some(b) if b != b'"' && b != b'\\' && b >= 0x20) {
                self.at += 1;
            }
            // The text is a `str` and the run stops only at ASCII bytes, so it
            // is whole UTF-8.
            out.push_str(std::str::from_utf8(&self.bytes[run..self.at]).expect("UTF-8"));
            match self.peek() {
                Some(b'"') => {
                    self.at += 1;
                    return Ok(out);
                }
                Some(b'\\') => {
                    self.at += 1;
                    self.escape(&mut out)?;
                }
                Some(_) => return Err(self.error("a control character in a string")),
                None => return Err(self.error("the text ends inside a string")),
            }
        }
    }

    fn escape(&mut self, out: &mut String) -> Result<(), JsonError> {
        let Some(byte) = self.peek() else {
            return Err(self.error("the text ends inside an escape"));
        };
        self.at += 1;
        out.push(match byte {
            b'"' => '"',
            b'\\' => '\\',
            b'/' => '/',
            b'b' => '\u{8}',
            b'f' => '\u{c}',
            b'n' => '\n',
            b'r' => '\r',
            b't' => '\t',
            b'u' => {
                let first = self.unit()?;
                let scalar = match first {
                    0xd800..0xdc00 => {
                        if !(self.eat(b'\\') && self.eat(b'u')) {
                            return Err(self.error("a high surrogate alone"));
                        }
                        let second = self.unit()?;
                        if !(0xdc00..0xe000).contains(&second) {
                            return Err(self.error("a high surrogate alone"));
                        }
                        0x10000 + ((u32::from(first) - 0xd800) << 10) + (u32::from(second) - 0xdc00)
                    }
                    0xdc00..0xe000 => return Err(self.error("a low surrogate alone")),
                    unit => u32::from(unit),
                };
                char::from_u32(scalar).expect("a paired or plain scalar")
            }
            _ => return Err(self.error("not an escape")),
        });
        Ok(())
    }

    /// The UTF-16 unit of the four hex digits next.
    fn unit(&mut self) -> Result<u16, JsonError> {
        let mut unit = 0u16;
        for _ in 0..4 {
            let digit = self
                .peek()
                .and_then(|b| char::from(b).to_digit(16))
                .ok_or_else(|| self.error("an escape needs four hex digits"))?;
            unit = unit << 4 | digit as u16;
            self.at += 1;
        }
        Ok(unit)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::JsonWriter;

    #[test]
    fn a_document_reads_as_its_tree() {
        let text = r#" {"a": [1, -2.5e3, 0.25, true, false, null],
            "s": "q\"\\\/\b\f\n\r\t\u00e9\ud83d\ude00", "a": {}, "e": []} "#;
        let value = JsonValue::parse(text).unwrap();
        let JsonValue::Object(members) = &value else {
            panic!()
        };
        assert_eq!(members.len(), 4, "duplicates kept");
        assert_eq!(
            value.get("a").and_then(JsonValue::as_array).map(<[_]>::len),
            Some(6),
            "the first member"
        );
        assert_eq!(
            value.get("a").unwrap().as_array().unwrap()[1].as_f64(),
            Some(-2500.0)
        );
        assert_eq!(
            value.get("s").and_then(JsonValue::as_str),
            Some("q\"\\/\u{8}\u{c}\n\r\té😀")
        );
        assert_eq!(value.get("missing"), None);
    }

    #[test]
    fn what_the_writer_writes_reads_back() {
        let mut w = JsonWriter::new();
        w.begin_object();
        w.name("path");
        w.string("C:\\dir\n\"x\"\u{1}");
        w.name("n");
        w.int(-7);
        w.end_object();
        let value = JsonValue::parse(w.as_str()).unwrap();
        assert_eq!(
            value.get("path").and_then(JsonValue::as_str),
            Some("C:\\dir\n\"x\"\u{1}")
        );
        assert_eq!(value.get("n").and_then(JsonValue::as_f64), Some(-7.0));
    }

    #[test]
    fn what_is_not_json_is_refused_where_it_goes_wrong() {
        for (text, offset) in [
            ("", 0),
            ("[1,]", 3),
            ("{\"a\" 1}", 5),
            ("01", 1),
            ("1.", 2),
            ("1e", 2),
            ("-", 1),
            ("\"\\x\"", 3),
            ("\"\\ud800\"", 7),
            ("\"\\udc00\"", 7),
            ("\"a\u{1}\"", 2),
            ("\"open", 5),
            ("tru", 0),
            ("[] []", 3),
            ("{,}", 1),
        ] {
            let error = JsonValue::parse(text).expect_err(text);
            assert_eq!(error.offset, offset, "{text:?}: {error}");
        }
        let deep = "[".repeat(MAX_DEPTH) + &"]".repeat(MAX_DEPTH);
        assert!(JsonValue::parse(&deep).is_ok());
        let deeper = "[".repeat(MAX_DEPTH + 1) + &"]".repeat(MAX_DEPTH + 1);
        assert_eq!(
            JsonValue::parse(&deeper).unwrap_err().message,
            "nested too deep"
        );
    }
}
