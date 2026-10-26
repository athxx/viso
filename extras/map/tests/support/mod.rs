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

use viso_map::TileId;
use viso_map::mvt::{GeomType, TileEncoder};

/// A made-up tile of `tile`, the same everywhere it overlaps another: a
/// world of lakes, parks and a grid of roads laid out in world units, so
/// tiles of every zoom meet without a seam and a picture shows how the view
/// placed them. Each kind of feature appears only from the zoom where it is
/// a few points across.
pub fn synthetic(tile: TileId) -> Vec<u8> {
    let z = i32::from(tile.z);
    let mut t = TileEncoder::new();
    let to_tile = |gx: f64, grid: i32| -> i32 {
        // Grid line `gx` of a grid of 2^grid cells in tile units.
        let k = f64::from(2i32.pow((z - grid).max(0) as u32))
            / f64::from(2i32.pow((grid - z).max(0) as u32));
        ((gx * k - f64::from(tile.x)) * 4096.0).round() as i32
    };
    let to_tile_y = |gy: f64, grid: i32| -> i32 {
        let k = f64::from(2i32.pow((z - grid).max(0) as u32))
            / f64::from(2i32.pow((grid - z).max(0) as u32));
        ((gy * k - f64::from(tile.y)) * 4096.0).round() as i32
    };
    // The cells of a grid of 2^grid that touch the tile, one either side.
    let cells = |grid: i32| -> (std::ops::Range<i64>, std::ops::Range<i64>) {
        let (x, y) = (i64::from(tile.x), i64::from(tile.y));
        if grid >= z {
            let n = 1i64 << (grid - z);
            (
                (x * n - 1)..((x + 1) * n + 1),
                (y * n - 1)..((y + 1) * n + 1),
            )
        } else {
            let d = z - grid;
            ((x >> d) - 1..(x >> d) + 2, (y >> d) - 1..(y >> d) + 2)
        }
    };
    let rect = |grid: i32, i: i64, j: i64, inset: f64| -> Vec<[i32; 2]> {
        let (a, b) = (i as f64 + inset, i as f64 + 1.0 - inset);
        let (c, d) = (j as f64 + inset, j as f64 + 1.0 - inset);
        vec![
            [to_tile(a, grid), to_tile_y(c, grid)],
            [to_tile(b, grid), to_tile_y(c, grid)],
            [to_tile(b, grid), to_tile_y(d, grid)],
            [to_tile(a, grid), to_tile_y(d, grid)],
        ]
    };
    if z >= 5 {
        let mut water = t.layer("water", 4096);
        let (xs, ys) = cells(10);
        for i in xs {
            for j in ys.clone() {
                if (i * 3 + j * 5).rem_euclid(11) == 0 {
                    let ring = rect(10, i, j, 0.12);
                    water.feature(
                        None,
                        GeomType::Polygon,
                        &[&ring],
                        &[("class", "lake".into())],
                    );
                }
            }
        }
        water.finish();
    }
    if z >= 8 {
        let mut park = t.layer("park", 4096);
        let (xs, ys) = cells(12);
        for i in xs {
            for j in ys.clone() {
                if (i + 2 * j).rem_euclid(5) == 0 {
                    let ring = rect(12, i, j, 0.25);
                    park.feature(
                        None,
                        GeomType::Polygon,
                        &[&ring],
                        &[("class", "park".into())],
                    );
                }
            }
        }
        park.finish();
    }
    if z >= 7 {
        let mut roads = t.layer("transportation", 4096);
        let (lo, hi) = (-256, 4096 + 256);
        for (grid, class, from) in [(11, "primary", 7), (13, "minor", 12)] {
            if z < from {
                continue;
            }
            let (xs, ys) = cells(grid);
            for i in xs {
                let x = to_tile(i as f64, grid);
                if (lo..hi).contains(&x) {
                    roads.feature(
                        None,
                        GeomType::Line,
                        &[&[[x, lo], [x, hi]]],
                        &[("class", class.into())],
                    );
                }
            }
            for j in ys {
                let y = to_tile_y(j as f64, grid);
                if (lo..hi).contains(&y) {
                    roads.feature(
                        None,
                        GeomType::Line,
                        &[&[[lo, y], [hi, y]]],
                        &[("class", class.into())],
                    );
                }
            }
        }
        roads.finish();
    }
    t.finish()
}
