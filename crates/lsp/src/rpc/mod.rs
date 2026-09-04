//! The JSON-RPC transport: a self-contained JSON value ([`json`]) plus
//! `Content-Length` message framing over synchronous byte streams.
//!
//! The language-server protocol frames each message as an HTTP-like header block
//! (`Content-Length: N\r\n\r\n`) followed by `N` bytes of JSON. This module reads and
//! writes that framing over any [`std::io::BufRead`]/[`std::io::Write`] — there is no
//! async runtime and no transport library (AGENTS 25). The server bin wires stdin/
//! stdout to it; tests wire in-memory buffers.

pub mod json;

use std::io::{BufRead, Write};

pub use json::Json;

/// Reads one `Content-Length`-framed message from `reader`, returning its parsed
/// JSON body. Returns `Ok(None)` at a clean end of stream (no more messages).
///
/// Malformed framing (a bad header, a truncated body, or unparseable JSON) is an
/// [`std::io::ErrorKind::InvalidData`] error — the caller shuts down rather than
/// trying to resynchronize a corrupt stream.
pub fn read_message<R: BufRead>(reader: &mut R) -> std::io::Result<Option<Json>> {
    let mut content_length: Option<usize> = None;
    let mut line = String::new();
    loop {
        line.clear();
        let n = reader.read_line(&mut line)?;
        if n == 0 {
            // End of stream. A pending header with no body is treated as clean EOF.
            return Ok(None);
        }
        let trimmed = line.trim_end_matches(['\r', '\n']);
        if trimmed.is_empty() {
            // The blank line ends the header block; the body follows.
            break;
        }
        if let Some(value) = trimmed.strip_prefix("Content-Length:") {
            content_length = value.trim().parse::<usize>().ok();
        }
        // Other headers (Content-Type) are accepted and ignored.
    }

    let len = content_length.ok_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "message header missing Content-Length",
        )
    })?;
    let mut body = vec![0u8; len];
    reader.read_exact(&mut body)?;
    let text = String::from_utf8(body).map_err(|_| {
        std::io::Error::new(std::io::ErrorKind::InvalidData, "message body is not UTF-8")
    })?;
    let value = json::parse(&text).ok_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "message body is not valid JSON",
        )
    })?;
    Ok(Some(value))
}

/// Writes one `Content-Length`-framed message: the header block, then the compact
/// JSON body.
pub fn write_message<W: Write>(writer: &mut W, message: &Json) -> std::io::Result<()> {
    let body = message.to_string();
    write!(writer, "Content-Length: {}\r\n\r\n", body.len())?;
    writer.write_all(body.as_bytes())?;
    writer.flush()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    #[test]
    fn reads_a_framed_message() {
        let body = r#"{"jsonrpc":"2.0","id":1,"method":"initialize"}"#;
        let framed = format!("Content-Length: {}\r\n\r\n{}", body.len(), body);
        let mut cursor = Cursor::new(framed.into_bytes());
        let msg = read_message(&mut cursor).unwrap().expect("one message");
        assert_eq!(msg.get("method").and_then(Json::as_str), Some("initialize"));
        // The stream is now exhausted.
        assert!(read_message(&mut cursor).unwrap().is_none());
    }

    #[test]
    fn write_then_read_round_trips() {
        let mut interner = std::collections::BTreeMap::new();
        interner.insert("hello".to_string(), Json::Bool(true));
        let msg = Json::Obj(interner);
        let mut buf = Vec::new();
        write_message(&mut buf, &msg).unwrap();
        let mut cursor = Cursor::new(buf);
        let back = read_message(&mut cursor).unwrap().expect("one message");
        assert_eq!(back, msg);
    }

    #[test]
    fn missing_content_length_is_an_error() {
        let framed = "X-Other: 1\r\n\r\n{}";
        let mut cursor = Cursor::new(framed.as_bytes().to_vec());
        assert!(read_message(&mut cursor).is_err());
    }
}
