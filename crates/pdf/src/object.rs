//! The file's objects: numbered, written as they are finished, and indexed by
//! the cross-reference table at the end.

use std::io::Write;

/// An indirect object's number.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) struct Ref(pub(crate) u32);

impl Ref {
    /// Appends ` n 0 R`.
    pub(crate) fn put(self, out: &mut Vec<u8>) {
        let _ = write!(out, "{} 0 R", self.0);
    }
}

/// Streams shorter than this are written as they are: compressing them
/// saves less than the filter entry costs.
const COMPRESS_FROM: usize = 64;

/// The bytes of the file so far and where each object starts.
pub(crate) struct Writer {
    buf: Vec<u8>,
    /// The byte offset of each object by number, `None` while unwritten.
    offsets: Vec<Option<usize>>,
}

impl Writer {
    pub(crate) fn new() -> Writer {
        let mut buf = Vec::with_capacity(64 * 1024);
        // The binary comment marks the file as binary to transfer tools.
        buf.extend_from_slice(b"%PDF-1.7\n%\xE2\xE3\xCF\xD3\n");
        Writer {
            buf,
            offsets: Vec::new(),
        }
    }

    /// Numbers an object to be written later.
    pub(crate) fn alloc(&mut self) -> Ref {
        self.offsets.push(None);
        Ref(self.offsets.len() as u32)
    }

    /// Writes object `at` with the body `body` writes.
    pub(crate) fn obj(&mut self, at: Ref, body: impl FnOnce(&mut Vec<u8>)) {
        self.offsets[at.0 as usize - 1] = Some(self.buf.len());
        let _ = writeln!(self.buf, "{} 0 obj", at.0);
        body(&mut self.buf);
        self.buf.extend_from_slice(b"\nendobj\n");
    }

    /// Writes object `at` as a stream of `data`, compressed when that pays,
    /// its dictionary holding the entries `entries` writes besides the
    /// length and filter.
    pub(crate) fn stream(&mut self, at: Ref, entries: impl FnOnce(&mut Vec<u8>), data: &[u8]) {
        let packed;
        let (data, filter) = if data.len() >= COMPRESS_FROM {
            packed = miniz_oxide::deflate::compress_to_vec_zlib(data, 6);
            (&packed[..], true)
        } else {
            (data, false)
        };
        self.raw_stream(at, entries, data, filter.then_some("/FlateDecode"));
    }

    /// Writes object `at` as a stream of `data` as given, with `filter` the
    /// filter it is already encoded with.
    pub(crate) fn raw_stream(
        &mut self,
        at: Ref,
        entries: impl FnOnce(&mut Vec<u8>),
        data: &[u8],
        filter: Option<&str>,
    ) {
        self.obj(at, |out| {
            out.extend_from_slice(b"<<");
            entries(out);
            let _ = write!(out, "/Length {}", data.len());
            if let Some(filter) = filter {
                let _ = write!(out, "/Filter{filter}");
            }
            out.extend_from_slice(b">>\nstream\n");
            out.extend_from_slice(data);
            out.extend_from_slice(b"\nendstream");
        });
    }

    /// The finished file: every allocated object written, then the
    /// cross-reference table and the trailer naming `root` and `info`.
    pub(crate) fn finish(mut self, root: Ref, info: Ref) -> Vec<u8> {
        let xref = self.buf.len();
        let count = self.offsets.len() + 1;
        let _ = write!(self.buf, "xref\n0 {count}\n0000000000 65535 f \n");
        for offset in &self.offsets {
            let offset = offset.expect("every allocated object is written");
            let _ = writeln!(self.buf, "{offset:010} 00000 n ");
        }
        // A file identifier from its own bytes, as viewers expect one.
        let id = fnv(&self.buf);
        let _ = write!(
            self.buf,
            "trailer\n<</Size {count}/Root {} 0 R/Info {} 0 R/ID[<{id:016x}{id:016x}><{id:016x}{id:016x}>]>>\nstartxref\n{xref}\n%%EOF\n",
            root.0, info.0,
        );
        self.buf
    }
}

/// FNV-1a over `bytes`.
fn fnv(bytes: &[u8]) -> u64 {
    bytes.iter().fold(0xcbf2_9ce4_8422_2325, |h, &b| {
        (h ^ u64::from(b)).wrapping_mul(0x0100_0000_01b3)
    })
}

/// Appends `v` as a PDF number: at most three decimals, no exponent, no
/// trailing zeros.
pub(crate) fn num(out: &mut Vec<u8>, v: f32) {
    let v = if v.is_finite() { v } else { 0.0 };
    let scaled = (f64::from(v) * 1000.0).round() as i64;
    if scaled == 0 {
        out.push(b'0');
        return;
    }
    if scaled < 0 {
        out.push(b'-');
    }
    let abs = scaled.unsigned_abs();
    let (int, frac) = (abs / 1000, abs % 1000);
    let _ = write!(out, "{int}");
    if frac != 0 {
        let mut digits = format!("{frac:03}");
        while digits.ends_with('0') {
            digits.pop();
        }
        out.push(b'.');
        out.extend_from_slice(digits.as_bytes());
    }
}

/// Appends `s` as a text string: literal when it is printable ASCII, UTF-16
/// with a byte-order mark otherwise.
pub(crate) fn text(out: &mut Vec<u8>, s: &str) {
    if s.bytes().all(|b| (0x20..0x7f).contains(&b)) {
        out.push(b'(');
        for b in s.bytes() {
            if matches!(b, b'(' | b')' | b'\\') {
                out.push(b'\\');
            }
            out.push(b);
        }
        out.push(b')');
    } else {
        out.extend_from_slice(b"<FEFF");
        for unit in s.encode_utf16() {
            let _ = write!(out, "{unit:04X}");
        }
        out.push(b'>');
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn n(v: f32) -> String {
        let mut out = Vec::new();
        num(&mut out, v);
        String::from_utf8(out).unwrap()
    }

    #[test]
    fn numbers_are_short_and_exact_to_three_places() {
        assert_eq!(n(0.0), "0");
        assert_eq!(n(-0.0001), "0");
        assert_eq!(n(12.5), "12.5");
        assert_eq!(n(-3.25), "-3.25");
        assert_eq!(n(0.1234), "0.123");
        assert_eq!(n(100.0), "100");
        assert_eq!(n(f32::NAN), "0");
    }

    #[test]
    fn text_strings_escape_or_go_utf16() {
        let mut out = Vec::new();
        text(&mut out, "a(b)\\");
        assert_eq!(out, b"(a\\(b\\)\\\\)");
        out.clear();
        text(&mut out, "é");
        assert_eq!(out, b"<FEFF00E9>");
    }
}
