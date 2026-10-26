//! Base64 (RFC 4648): bytes written as text where only text travels — data
//! URLs, a glTF file's embedded buffers, a page's inlined assets.

use alloc::string::String;
use alloc::vec::Vec;

/// The standard alphabet.
const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

/// `bytes` in the standard alphabet, padded to a multiple of four.
pub fn encode(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let n = (u32::from(chunk[0]) << 16)
            | (u32::from(*chunk.get(1).unwrap_or(&0)) << 8)
            | u32::from(*chunk.get(2).unwrap_or(&0));
        for i in 0..4 {
            if i <= chunk.len() {
                out.push(char::from(ALPHABET[((n >> (18 - 6 * i)) & 63) as usize]));
            } else {
                out.push('=');
            }
        }
    }
    out
}

/// The bytes `text` encodes, in the standard or the URL-safe alphabet:
/// ASCII whitespace is skipped and the padding may be left off; the first
/// `=` ends the text. `None` at any other byte.
pub fn decode(text: &[u8]) -> Option<Vec<u8>> {
    let mut out = Vec::with_capacity(text.len() / 4 * 3 + 2);
    let (mut acc, mut bits) = (0u32, 0u32);
    for &c in text {
        let value = match c {
            b'A'..=b'Z' => c - b'A',
            b'a'..=b'z' => c - b'a' + 26,
            b'0'..=b'9' => c - b'0' + 52,
            b'+' | b'-' => 62,
            b'/' | b'_' => 63,
            b'=' => break,
            c if c.is_ascii_whitespace() => continue,
            _ => return None,
        };
        acc = (acc << 6) | u32::from(value);
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((acc >> bits) as u8);
            acc &= (1 << bits) - 1;
        }
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bytes_round_trip_padded() {
        assert_eq!(encode(b""), "");
        assert_eq!(encode(b"f"), "Zg==");
        assert_eq!(encode(b"fo"), "Zm8=");
        assert_eq!(encode(b"foo"), "Zm9v");
        assert_eq!(encode(b"foobar"), "Zm9vYmFy");
        let all: Vec<u8> = (0..=255).collect();
        assert_eq!(decode(encode(&all).as_bytes()).as_deref(), Some(&all[..]));
    }

    #[test]
    fn decoding_takes_either_alphabet_whitespace_and_no_padding() {
        assert_eq!(decode(b"Zm9vYg==").as_deref(), Some(&b"foob"[..]));
        assert_eq!(decode(b"Zm9vYg").as_deref(), Some(&b"foob"[..]));
        assert_eq!(decode(b"Zm9v\n YmFy").as_deref(), Some(&b"foobar"[..]));
        assert_eq!(decode(b"-_8=").as_deref(), decode(b"+/8=").as_deref());
        assert_eq!(decode(b"Zm9v*"), None);
    }
}
