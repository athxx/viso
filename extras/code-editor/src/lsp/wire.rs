//! What goes over the wire to a language server: `Content-Length` frames
//! of JSON, and positions as lines and columns in the encoding the server
//! agreed to.

use std::io::{self, BufRead, Write};
use std::ops::Range;

use viso_ende::{JsonValue, JsonWriter};

use crate::rope::Rope;

/// A JSON value to send.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum J {
    Null,
    Bool(bool),
    Int(i64),
    Str(String),
    Arr(Vec<J>),
    Obj(Vec<(&'static str, J)>),
}

impl J {
    pub fn str(s: impl Into<String>) -> J {
        J::Str(s.into())
    }

    /// It as compact JSON.
    pub fn encode(&self) -> String {
        let mut w = JsonWriter::new();
        self.write(&mut w);
        w.into_string()
    }

    fn write(&self, w: &mut JsonWriter) {
        match self {
            J::Null => w.null(),
            J::Bool(b) => w.bool(*b),
            J::Int(n) => w.int(*n),
            J::Str(s) => w.string(s),
            J::Arr(items) => {
                w.begin_array();
                for i in items {
                    i.write(w);
                }
                w.end_array();
            }
            J::Obj(members) => {
                w.begin_object();
                for (k, v) in members {
                    w.name(k);
                    v.write(w);
                }
                w.end_object();
            }
        }
    }
}

/// An object of `members`.
pub(crate) fn obj<const N: usize>(members: [(&'static str, J); N]) -> J {
    J::Obj(members.into())
}

/// A request.
pub(crate) fn request(id: i64, method: &str, params: J) -> String {
    obj([
        ("jsonrpc", J::str("2.0")),
        ("id", J::Int(id)),
        ("method", J::str(method)),
        ("params", params),
    ])
    .encode()
}

/// A notification.
pub(crate) fn notification(method: &str, params: J) -> String {
    obj([
        ("jsonrpc", J::str("2.0")),
        ("method", J::str(method)),
        ("params", params),
    ])
    .encode()
}

/// The reply to the server's request `id`, already encoded.
pub(crate) fn response(id: &JsonValue, result: J) -> String {
    let id = match id {
        JsonValue::Number(n) => J::Int(*n as i64),
        JsonValue::String(s) => J::Str(s.clone()),
        _ => J::Null,
    };
    obj([("jsonrpc", J::str("2.0")), ("id", id), ("result", result)]).encode()
}

/// Writes `body` as one frame.
pub(crate) fn write_frame(out: &mut impl Write, body: &str) -> io::Result<()> {
    write!(out, "Content-Length: {}\r\n\r\n", body.len())?;
    out.write_all(body.as_bytes())?;
    out.flush()
}

/// Reads one frame's body; `None` at the end of the stream.
pub(crate) fn read_frame(input: &mut impl BufRead) -> io::Result<Option<String>> {
    let mut len = None;
    let mut line = String::new();
    loop {
        line.clear();
        if input.read_line(&mut line)? == 0 {
            return Ok(None);
        }
        let header = line.trim_end();
        if header.is_empty() {
            if len.is_some() {
                break;
            }
            continue;
        }
        if let Some((name, value)) = header.split_once(':')
            && name.trim().eq_ignore_ascii_case("content-length")
        {
            len = value.trim().parse::<usize>().ok();
        }
    }
    let mut body = vec![0; len.unwrap_or(0)];
    input.read_exact(&mut body)?;
    Ok(Some(String::from_utf8_lossy(&body).into_owned()))
}

/// How a server counts a column.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Encoding {
    Utf8,
    #[default]
    Utf16,
}

/// Byte `at` of `text` as a position.
pub(crate) fn position(text: &Rope, at: usize, enc: Encoding) -> J {
    let at = at.min(text.len());
    let line = text.line_of(at);
    let start = text.line_start(line);
    let column = match enc {
        Encoding::Utf8 => at - start,
        Encoding::Utf16 => text.slice(start..at).encode_utf16().count(),
    };
    obj([
        ("line", J::Int(line as i64)),
        ("character", J::Int(column as i64)),
    ])
}

/// Bytes `range` of `text` as a range.
pub(crate) fn range(text: &Rope, range: Range<usize>, enc: Encoding) -> J {
    obj([
        ("start", position(text, range.start, enc)),
        ("end", position(text, range.end, enc)),
    ])
}

/// The byte of `text` position `p` names, kept within its line.
pub(crate) fn offset(text: &Rope, p: &JsonValue, enc: Encoding) -> Option<usize> {
    let line = p.get("line")?.as_f64()? as usize;
    let column = p.get("character")?.as_f64()? as usize;
    if line >= text.line_count() {
        return Some(text.len());
    }
    let start = text.line_start(line);
    let s = text.line(line);
    let byte = match enc {
        Encoding::Utf8 => {
            let mut b = column.min(s.len());
            while !s.is_char_boundary(b) {
                b -= 1;
            }
            b
        }
        Encoding::Utf16 => {
            let mut units = 0;
            let mut b = s.len();
            for (i, c) in s.char_indices() {
                if units >= column {
                    b = i;
                    break;
                }
                units += c.len_utf16();
            }
            b
        }
    };
    Some(start + byte)
}

/// The bytes of `text` range `r` names.
pub(crate) fn span(text: &Rope, r: &JsonValue, enc: Encoding) -> Option<Range<usize>> {
    let a = offset(text, r.get("start")?, enc)?;
    let b = offset(text, r.get("end")?, enc)?;
    Some(a.min(b)..b.max(a))
}

/// A `file://` URI for `path`, made absolute.
pub fn file_uri(path: &std::path::Path) -> String {
    let path = std::path::absolute(path).unwrap_or_else(|_| path.to_path_buf());
    let mut out = String::from("file://");
    let s = path.to_string_lossy().replace('\\', "/");
    if !s.starts_with('/') {
        out.push('/');
    }
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'/' | b'-' | b'_' | b'.' | b'~' | b':' => {
                out.push(b as char)
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

/// `uri` with its escapes read, and a drive letter's case folded: two
/// spellings of one file compare equal.
pub(crate) fn uri_key(uri: &str) -> String {
    let bytes = uri.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%'
            && i + 2 < bytes.len()
            && let Ok(b) = u8::from_str_radix(&uri[i + 1..i + 3], 16)
        {
            out.push(b);
            i += 3;
            continue;
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).to_lowercase()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn positions_count_columns_in_either_encoding() {
        let text = Rope::from("ab\n日本x\n");
        let at = 3 + "日本".len();
        assert_eq!(
            position(&text, at, Encoding::Utf16).encode(),
            r#"{"line":1,"character":2}"#
        );
        assert_eq!(
            position(&text, at, Encoding::Utf8).encode(),
            r#"{"line":1,"character":6}"#
        );
        let p = JsonValue::parse(r#"{"line":1,"character":2}"#).unwrap();
        assert_eq!(offset(&text, &p, Encoding::Utf16), Some(at));
        let p = JsonValue::parse(r#"{"line":1,"character":4}"#).unwrap();
        assert_eq!(
            offset(&text, &p, Encoding::Utf8),
            Some(6),
            "inside a character"
        );
        let p = JsonValue::parse(r#"{"line":9,"character":0}"#).unwrap();
        assert_eq!(offset(&text, &p, Encoding::Utf16), Some(text.len()));
    }

    #[test]
    fn frames_go_out_and_come_back() {
        let mut buf = Vec::new();
        write_frame(&mut buf, r#"{"a":1}"#).unwrap();
        write_frame(&mut buf, "{}").unwrap();
        let mut r = std::io::BufReader::new(&buf[..]);
        assert_eq!(read_frame(&mut r).unwrap().as_deref(), Some(r#"{"a":1}"#));
        assert_eq!(read_frame(&mut r).unwrap().as_deref(), Some("{}"));
        assert_eq!(read_frame(&mut r).unwrap(), None);
    }

    #[test]
    fn uris_escape_and_compare() {
        let uri = file_uri(std::path::Path::new("/tmp/a b/c.vs"));
        assert_eq!(uri, "file:///tmp/a%20b/c.vs");
        assert_eq!(uri_key(&uri), uri_key("file:///tmp/a b/c.vs"));
    }
}
