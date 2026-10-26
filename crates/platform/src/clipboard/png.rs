//! The PNG file an image of pixels goes on the clipboard as: 8-bit RGBA, each
//! row filtered as best compresses it, deflated row by row so no filtered
//! copy of the whole image is held.

use miniz_oxide::deflate::core::{
    CompressorOxide, TDEFLFlush, TDEFLStatus, compress, create_comp_flags_from_zip_params,
};

const SIGNATURE: &[u8; 8] = b"\x89PNG\r\n\x1a\n";

/// Whether `bytes` are a PNG file (by its signature alone).
pub(crate) fn is_png(bytes: &[u8]) -> bool {
    bytes.starts_with(SIGNATURE)
}

/// `width × height` straight-alpha RGBA8 `pixels`, rows top first, as a PNG
/// file.
pub(crate) fn encode(width: u32, height: u32, pixels: &[u8]) -> Vec<u8> {
    let stride = width as usize * 4;
    debug_assert_eq!(pixels.len(), stride * height as usize);
    // A fast level: what is pasted is decoded once, and the copy waits on it.
    let mut deflater = Box::new(CompressorOxide::new(create_comp_flags_from_zip_params(
        3, 1, 0,
    )));
    let mut data = Vec::with_capacity(pixels.len() / 4 + 64);
    let mut row = vec![0u8; stride + 1];
    let mut candidate = vec![0u8; stride];
    let zero = vec![0u8; stride];
    let mut scratch = vec![0u8; 64 * 1024];
    for (y, line) in pixels.chunks_exact(stride).enumerate() {
        let above = if y == 0 {
            &zero[..]
        } else {
            &pixels[(y - 1) * stride..y * stride]
        };
        let mut best_cost = u64::MAX;
        for filter in 0..5u8 {
            filter_row(filter, line, above, &mut candidate);
            let cost = candidate
                .iter()
                .map(|&b| u64::from((b as i8).unsigned_abs()))
                .sum();
            if cost < best_cost {
                best_cost = cost;
                row[0] = filter;
                row[1..].copy_from_slice(&candidate);
            }
        }
        deflate(
            &mut deflater,
            &row,
            &mut data,
            &mut scratch,
            TDEFLFlush::None,
        );
    }
    deflate(
        &mut deflater,
        &[],
        &mut data,
        &mut scratch,
        TDEFLFlush::Finish,
    );

    let mut out = Vec::with_capacity(data.len() + 57);
    out.extend_from_slice(SIGNATURE);
    let mut header = [0u8; 13];
    header[..4].copy_from_slice(&width.to_be_bytes());
    header[4..8].copy_from_slice(&height.to_be_bytes());
    // 8 bits a channel, RGBA, deflate, adaptive filtering, no interlace.
    header[8..].copy_from_slice(&[8, 6, 0, 0, 0]);
    chunk(&mut out, b"IHDR", &header);
    chunk(&mut out, b"IDAT", &data);
    chunk(&mut out, b"IEND", &[]);
    out
}

/// Feed `input` to `deflater`, appending what it writes to `out` through
/// the window `scratch`.
fn deflate(
    deflater: &mut CompressorOxide,
    mut input: &[u8],
    out: &mut Vec<u8>,
    scratch: &mut [u8],
    flush: TDEFLFlush,
) {
    loop {
        let (status, read, written) = compress(deflater, input, scratch, flush);
        out.extend_from_slice(&scratch[..written]);
        input = &input[read..];
        match status {
            TDEFLStatus::Done => return,
            // All taken, and room left: what the deflater holds back goes
            // out with a later row.
            TDEFLStatus::Okay
                if input.is_empty() && flush != TDEFLFlush::Finish && written < scratch.len() =>
            {
                return;
            }
            TDEFLStatus::Okay => {}
            // Only a bad parameter or a failed callback, neither used here.
            _ => return,
        }
    }
}

/// Row `line` filtered with PNG filter `filter` against the row `above` it.
fn filter_row(filter: u8, line: &[u8], above: &[u8], out: &mut [u8]) {
    for i in 0..line.len() {
        let a = if i >= 4 { line[i - 4] } else { 0 };
        let b = above[i];
        let c = if i >= 4 { above[i - 4] } else { 0 };
        out[i] = line[i].wrapping_sub(match filter {
            0 => 0,
            1 => a,
            2 => b,
            3 => ((u16::from(a) + u16::from(b)) / 2) as u8,
            _ => paeth(a, b, c),
        });
    }
}

fn paeth(a: u8, b: u8, c: u8) -> u8 {
    let p = i16::from(a) + i16::from(b) - i16::from(c);
    let (pa, pb, pc) = (
        (p - i16::from(a)).abs(),
        (p - i16::from(b)).abs(),
        (p - i16::from(c)).abs(),
    );
    if pa <= pb && pa <= pc {
        a
    } else if pb <= pc {
        b
    } else {
        c
    }
}

fn chunk(out: &mut Vec<u8>, kind: &[u8; 4], data: &[u8]) {
    out.extend_from_slice(&(data.len() as u32).to_be_bytes());
    let start = out.len();
    out.extend_from_slice(kind);
    out.extend_from_slice(data);
    let crc = crc32(&out[start..]);
    out.extend_from_slice(&crc.to_be_bytes());
}

/// The CRC-32 PNG chunks carry.
fn crc32(bytes: &[u8]) -> u32 {
    const TABLE: [u32; 256] = {
        let mut table = [0u32; 256];
        let mut n = 0;
        while n < 256 {
            let mut c = n as u32;
            let mut k = 0;
            while k < 8 {
                c = if c & 1 != 0 {
                    0xEDB8_8320 ^ (c >> 1)
                } else {
                    c >> 1
                };
                k += 1;
            }
            table[n] = c;
            n += 1;
        }
        table
    };
    let mut crc = !0u32;
    for &b in bytes {
        crc = TABLE[((crc ^ u32::from(b)) & 0xFF) as usize] ^ (crc >> 8);
    }
    !crc
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The pixels a PNG this encoder wrote holds.
    fn decode(png: &[u8]) -> (u32, u32, Vec<u8>) {
        assert!(is_png(png));
        let mut at = 8;
        let (mut width, mut height, mut data) = (0, 0, Vec::new());
        while at < png.len() {
            let len = u32::from_be_bytes(png[at..at + 4].try_into().unwrap()) as usize;
            let kind = &png[at + 4..at + 8];
            let body = &png[at + 8..at + 8 + len];
            let crc = u32::from_be_bytes(png[at + 8 + len..at + 12 + len].try_into().unwrap());
            assert_eq!(crc, crc32(&png[at + 4..at + 8 + len]));
            match kind {
                b"IHDR" => {
                    width = u32::from_be_bytes(body[..4].try_into().unwrap());
                    height = u32::from_be_bytes(body[4..8].try_into().unwrap());
                    assert_eq!(&body[8..], &[8, 6, 0, 0, 0]);
                }
                b"IDAT" => data.extend_from_slice(body),
                _ => {}
            }
            at += 12 + len;
        }
        let raw = miniz_oxide::inflate::decompress_to_vec_zlib(&data).unwrap();
        let stride = width as usize * 4;
        let mut pixels = vec![0u8; stride * height as usize];
        for y in 0..height as usize {
            let row = &raw[y * (stride + 1)..(y + 1) * (stride + 1)];
            for i in 0..stride {
                let a = if i >= 4 {
                    pixels[y * stride + i - 4]
                } else {
                    0
                };
                let b = if y > 0 {
                    pixels[(y - 1) * stride + i]
                } else {
                    0
                };
                let c = if i >= 4 && y > 0 {
                    pixels[(y - 1) * stride + i - 4]
                } else {
                    0
                };
                let base = match row[0] {
                    0 => 0,
                    1 => a,
                    2 => b,
                    3 => ((u16::from(a) + u16::from(b)) / 2) as u8,
                    _ => paeth(a, b, c),
                };
                pixels[y * stride + i] = row[1 + i].wrapping_add(base);
            }
        }
        (width, height, pixels)
    }

    #[test]
    fn pixels_round_trip_through_the_file() {
        let (w, h) = (37u32, 23u32);
        let pixels: Vec<u8> = (0..w * h * 4)
            .map(|i| (i.wrapping_mul(2_654_435_761) >> 13) as u8 ^ (i / 7) as u8)
            .collect();
        let png = encode(w, h, &pixels);
        assert_eq!(decode(&png), (w, h, pixels));
    }

    #[test]
    fn a_flat_image_compresses() {
        let pixels = vec![200u8; 512 * 512 * 4];
        let png = encode(512, 512, &pixels);
        assert!(png.len() < 8 * 1024, "{} bytes", png.len());
        assert_eq!(decode(&png).2, pixels);
    }
}
