//! Helpers the map's tests share: PNG output of BGRA frames, and the
//! directories tests read tiles from and write pictures to.

#![allow(dead_code)]

use std::path::PathBuf;

/// The CRC-32 a PNG chunk carries.
fn crc32(bytes: &[u8]) -> u32 {
    let mut crc = !0u32;
    for &b in bytes {
        crc ^= u32::from(b);
        for _ in 0..8 {
            crc = (crc >> 1) ^ (0xedb8_8320 & (!(crc & 1)).wrapping_add(1));
        }
    }
    !crc
}

/// A PNG of a `w` × `h` frame of BGRA bytes.
pub fn png(w: u32, h: u32, bgra: &[u8]) -> Vec<u8> {
    let mut raw = Vec::with_capacity((w * h * 4 + h) as usize);
    for row in bgra.chunks_exact(w as usize * 4) {
        raw.push(0);
        for px in row.as_chunks::<4>().0 {
            raw.extend_from_slice(&[px[2], px[1], px[0], px[3]]);
        }
    }
    let mut out = vec![0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a];
    let mut chunk = |kind: &[u8; 4], data: &[u8]| {
        out.extend_from_slice(&(data.len() as u32).to_be_bytes());
        let mut body = kind.to_vec();
        body.extend_from_slice(data);
        out.extend_from_slice(&body);
        out.extend_from_slice(&crc32(&body).to_be_bytes());
    };
    let mut ihdr = Vec::new();
    ihdr.extend_from_slice(&w.to_be_bytes());
    ihdr.extend_from_slice(&h.to_be_bytes());
    ihdr.extend_from_slice(&[8, 6, 0, 0, 0]);
    chunk(b"IHDR", &ihdr);
    chunk(
        b"IDAT",
        &miniz_oxide::deflate::compress_to_vec_zlib(&raw, 6),
    );
    chunk(b"IEND", &[]);
    out
}

/// Where pictures go: `VISO_MAP_OUT`, when set.
pub fn out_dir() -> Option<PathBuf> {
    let dir = PathBuf::from(std::env::var_os("VISO_MAP_OUT")?);
    std::fs::create_dir_all(&dir).ok()?;
    Some(dir)
}

/// The real tiles to read: `VISO_MAP_TILES`, when set.
pub fn tiles_dir() -> Option<PathBuf> {
    std::env::var_os("VISO_MAP_TILES").map(PathBuf::from)
}

/// Write `bgra` as `name.png` in the output directory, if there is one.
pub fn save(name: &str, w: u32, h: u32, bgra: &[u8]) {
    if let Some(dir) = out_dir() {
        std::fs::write(dir.join(format!("{name}.png")), png(w, h, bgra)).unwrap();
    }
}
