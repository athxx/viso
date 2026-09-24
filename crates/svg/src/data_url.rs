//! `data:` URLs (RFC 2397) and raster image header sniffing.

use std::sync::Arc;

use crate::tree::ImageKind;

pub(crate) struct DataUrl {
    pub mime: String,
    pub bytes: Vec<u8>,
}

/// Decode a `data:[<mime>][;base64],<payload>` URL. Other URLs yield `None`
/// (external resources are not loaded).
pub(crate) fn decode(href: &str) -> Option<DataUrl> {
    let href = href.trim();
    let rest = href
        .get(..5)
        .filter(|p| p.eq_ignore_ascii_case("data:"))
        .map(|_| &href[5..])?;
    let comma = rest.find(',')?;
    let (meta, payload) = (&rest[..comma], &rest[comma + 1..]);
    let mut parts = meta.split(';').map(str::trim);
    let mime = parts.next().unwrap_or("").to_ascii_lowercase();
    let base64 = parts.any(|p| p.eq_ignore_ascii_case("base64"));
    let raw = percent_decode(payload);
    let bytes = if base64 { base64_decode(&raw)? } else { raw };
    Some(DataUrl { mime, bytes })
}

fn percent_decode(s: &str) -> Vec<u8> {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%'
            && i + 2 < b.len()
            && let (Some(h), Some(l)) = (hex(b[i + 1]), hex(b[i + 2]))
        {
            out.push(h << 4 | l);
            i += 3;
            continue;
        }
        out.push(b[i]);
        i += 1;
    }
    out
}

fn hex(c: u8) -> Option<u8> {
    match c {
        b'0'..=b'9' => Some(c - b'0'),
        b'a'..=b'f' => Some(c - b'a' + 10),
        b'A'..=b'F' => Some(c - b'A' + 10),
        _ => None,
    }
}

/// Standard and URL-safe alphabets; whitespace is skipped, padding optional.
fn base64_decode(s: &[u8]) -> Option<Vec<u8>> {
    let mut out = Vec::with_capacity(s.len() * 3 / 4);
    let mut acc = 0u32;
    let mut bits = 0u32;
    for &c in s {
        let v = match c {
            b'A'..=b'Z' => c - b'A',
            b'a'..=b'z' => c - b'a' + 26,
            b'0'..=b'9' => c - b'0' + 52,
            b'+' | b'-' => 62,
            b'/' | b'_' => 63,
            b'=' => break,
            c if c.is_ascii_whitespace() => continue,
            _ => return None,
        };
        acc = acc << 6 | v as u32;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((acc >> bits) as u8);
        }
    }
    Some(out)
}

/// Identify a PNG/JPEG/GIF/WebP payload and read its pixel size.
pub(crate) fn sniff_image(bytes: Vec<u8>) -> Option<(ImageKind, (u32, u32))> {
    let b = bytes.as_slice();
    let be32 = |i: usize| Some(u32::from_be_bytes(b.get(i..i + 4)?.try_into().ok()?));
    let le16 = |i: usize| Some(u16::from_le_bytes([*b.get(i)?, *b.get(i + 1)?]) as u32);
    let le24 = |i: usize| Some(le16(i)? | (*b.get(i + 2)? as u32) << 16);

    let size = if b.starts_with(b"\x89PNG\r\n\x1a\n") {
        // IHDR is the first chunk.
        (b.get(12..16)? == b"IHDR").then_some(())?;
        (0, (be32(16)?, be32(20)?))
    } else if b.starts_with(b"GIF87a") || b.starts_with(b"GIF89a") {
        (1, (le16(6)?, le16(8)?))
    } else if b.starts_with(&[0xFF, 0xD8]) {
        (2, jpeg_size(b)?)
    } else if b.len() >= 16 && &b[0..4] == b"RIFF" && &b[8..12] == b"WEBP" {
        let dims = match &b[12..16] {
            b"VP8 " => (le16(26)? & 0x3FFF, le16(28)? & 0x3FFF),
            b"VP8L" => {
                (*b.get(20)? == 0x2F).then_some(())?;
                let v = u32::from_le_bytes(b.get(21..25)?.try_into().ok()?);
                ((v & 0x3FFF) + 1, ((v >> 14) & 0x3FFF) + 1)
            }
            b"VP8X" => (le24(24)? + 1, le24(27)? + 1),
            _ => return None,
        };
        (3, dims)
    } else {
        return None;
    };
    let kind = match size.0 {
        0 => ImageKind::Png(Arc::new(bytes)),
        1 => ImageKind::Gif(Arc::new(bytes)),
        2 => ImageKind::Jpeg(Arc::new(bytes)),
        _ => ImageKind::Webp(Arc::new(bytes)),
    };
    Some((kind, size.1))
}

/// Walk JPEG markers to the first SOFn frame header.
fn jpeg_size(b: &[u8]) -> Option<(u32, u32)> {
    let mut i = 2;
    loop {
        while *b.get(i)? != 0xFF {
            i += 1;
        }
        while *b.get(i)? == 0xFF {
            i += 1;
        }
        let m = *b.get(i)?;
        i += 1;
        match m {
            0xD8 | 0x01 | 0xD0..=0xD7 => continue,
            0xD9 | 0xDA => return None,
            _ => {}
        }
        let len = u16::from_be_bytes([*b.get(i)?, *b.get(i + 1)?]) as usize;
        if matches!(m, 0xC0..=0xCF) && !matches!(m, 0xC4 | 0xC8 | 0xCC) {
            let h = u16::from_be_bytes([*b.get(i + 3)?, *b.get(i + 4)?]) as u32;
            let w = u16::from_be_bytes([*b.get(i + 5)?, *b.get(i + 6)?]) as u32;
            return Some((w, h));
        }
        i += len;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_base64_and_percent() {
        let d = decode("data:image/svg+xml;base64,PHN2Zy8+").unwrap();
        assert_eq!(d.mime, "image/svg+xml");
        assert_eq!(d.bytes, b"<svg/>");
        assert_eq!(decode("data:,a%20b").unwrap().bytes, b"a b");
        assert!(decode("file.png").is_none());
    }

    #[test]
    fn sniffs_png_size() {
        let mut png = b"\x89PNG\r\n\x1a\n\0\0\0\rIHDR".to_vec();
        png.extend_from_slice(&7u32.to_be_bytes());
        png.extend_from_slice(&9u32.to_be_bytes());
        let (kind, size) = sniff_image(png).unwrap();
        assert!(matches!(kind, ImageKind::Png(_)));
        assert_eq!(size, (7, 9));
    }
}
