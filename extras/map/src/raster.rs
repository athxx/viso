//! Image tiles, worked out off the loop: a raster layer's PNG, JPEG or WebP
//! decoded, and a terrain tile's elevations decoded and shaded into a
//! hillshade.

use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use viso_render::asset::{RasterImage, decode_raster};
use viso_services::{Reply, ServiceError, reply};

use crate::geo::{EARTH_RADIUS, tile_square};
use crate::source::{TileId, TileSource};
use crate::style::{Color, DemEncoding};

/// A source of image tiles (PNG, JPEG or WebP) a style's raster and
/// hillshade layers read by name.
#[derive(Clone)]
pub struct RasterSource {
    pub(crate) source: Rc<dyn TileSource>,
    pub(crate) min_zoom: u8,
    pub(crate) max_zoom: u8,
    pub(crate) tile_size: u32,
}

impl std::fmt::Debug for RasterSource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RasterSource")
            .field("zooms", &(self.min_zoom, self.max_zoom))
            .field("tile_size", &self.tile_size)
            .finish_non_exhaustive()
    }
}

impl RasterSource {
    /// `source`'s tiles, of zooms `min_zoom..=max_zoom`, 256 pixels across.
    pub fn new(source: impl TileSource, min_zoom: u8, max_zoom: u8) -> Self {
        Self {
            source: Rc::new(source),
            min_zoom,
            max_zoom: max_zoom.max(min_zoom),
            tile_size: 256,
        }
    }

    /// Its tiles are `pixels` across (256 or 512): a tile of 256 shows at
    /// half a vector tile's size, a level deeper.
    pub fn tile_size(mut self, pixels: u32) -> Self {
        self.tile_size = pixels.max(1);
        self
    }

    /// The levels deeper than the vector tiles' its tiles are drawn at.
    pub(crate) fn depth(&self) -> u8 {
        let mut d = 0;
        let mut px = self.tile_size;
        while px < crate::camera::TILE_SIZE as u32 && d < 4 {
            px *= 2;
            d += 1;
        }
        d
    }
}

/// What an image layer makes of a tile's bytes.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum ImageKind {
    /// The image as it is.
    Image,
    /// Elevations, shaded.
    Shade(Shading),
}

/// `bytes` of `tile` made into an image by `kind` on the worker pool, and
/// the flag that stops it if raised before it starts.
pub fn image_off_loop(
    bytes: Vec<u8>,
    tile: TileId,
    kind: ImageKind,
) -> (Reply<Arc<RasterImage>>, Arc<AtomicBool>) {
    let (done, answer) = reply();
    let stop = Arc::new(AtomicBool::new(false));
    let stopped = Arc::clone(&stop);
    viso_ui::work::submit(move || {
        if stopped.load(Ordering::Relaxed) {
            done.complete(Err(ServiceError::Cancelled));
            return;
        }
        done.complete(match kind {
            ImageKind::Image => decode_image(tile, &bytes),
            ImageKind::Shade(shading) => decode_dem(tile, &bytes, shading.encoding)
                .and_then(|dem| hillshade(tile, &dem, &shading))
                .map(Arc::new),
        });
    });
    (answer, stop)
}

/// The elevations in `bytes` of terrain tile `tile`, decoded by `encoding`
/// on the worker pool, and the flag that stops it if raised before it
/// starts.
pub fn dem_off_loop(
    bytes: Vec<u8>,
    tile: TileId,
    encoding: DemEncoding,
) -> (Reply<Arc<Dem>>, Arc<AtomicBool>) {
    crate::load::off_loop(move || decode_dem(tile, &bytes, encoding).map(Arc::new))
}

/// How a hillshade shades.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Shading {
    pub encoding: DemEncoding,
    /// How strongly slopes shade, `0` to `1`.
    pub exaggeration: f32,
    pub shadow: Color,
    pub highlight: Color,
    /// Where the light comes from, degrees clockwise from north.
    pub illumination: f32,
}

/// A tile's elevations, in meters, rows from the north.
#[derive(Debug, Clone, PartialEq)]
pub struct Dem {
    pub width: u32,
    pub height: u32,
    pub meters: Vec<f32>,
}

impl Dem {
    /// The elevation at pixel `(x, y)`, clamped to the tile.
    #[inline]
    pub fn at(&self, x: i64, y: i64) -> f32 {
        let x = x.clamp(0, i64::from(self.width) - 1) as usize;
        let y = y.clamp(0, i64::from(self.height) - 1) as usize;
        self.meters[y * self.width as usize + x]
    }

    /// The elevation at `(u, v)`, `0..1` across and down the tile, between
    /// its pixels.
    pub fn sample(&self, u: f64, v: f64) -> f32 {
        let x = (u * f64::from(self.width) - 0.5).max(0.0);
        let y = (v * f64::from(self.height) - 0.5).max(0.0);
        let (x0, y0) = (x.floor() as i64, y.floor() as i64);
        let (fx, fy) = ((x - x0 as f64) as f32, (y - y0 as f64) as f32);
        let a = self.at(x0, y0) * (1.0 - fx) + self.at(x0 + 1, y0) * fx;
        let b = self.at(x0, y0 + 1) * (1.0 - fx) + self.at(x0 + 1, y0 + 1) * fx;
        a * (1.0 - fy) + b * fy
    }
}

fn failed(tile: TileId, e: impl std::fmt::Display) -> ServiceError {
    ServiceError::Failed(format!("tile {tile}: {e}"))
}

/// A raster layer's tile decoded.
pub fn decode_image(tile: TileId, bytes: &[u8]) -> Result<Arc<RasterImage>, ServiceError> {
    decode_raster(bytes)
        .map(Arc::new)
        .map_err(|e| failed(tile, e))
}

/// A terrain tile's elevations, read from its pixels by `encoding`.
pub fn decode_dem(tile: TileId, bytes: &[u8], encoding: DemEncoding) -> Result<Dem, ServiceError> {
    let image = decode_raster(bytes).map_err(|e| failed(tile, e))?;
    let pixels = image
        .to_straight_rgba8()
        .ok_or_else(|| failed(tile, "unreadable texels"))?;
    let meters = pixels
        .as_chunks::<4>()
        .0
        .iter()
        .map(|p| {
            let (r, g, b) = (f32::from(p[0]), f32::from(p[1]), f32::from(p[2]));
            match encoding {
                DemEncoding::Terrarium => r * 256.0 + g + b / 256.0 - 32768.0,
                DemEncoding::MapboxRgb => -10000.0 + (r * 65536.0 + g * 256.0 + b) * 0.1,
            }
        })
        .collect();
    Ok(Dem {
        width: image.width,
        height: image.height,
        meters,
    })
}

/// `dem` of `tile` shaded by `shading`: shadow on the slopes facing away
/// from the light, highlight on those facing it, clear on the flat.
pub fn hillshade(tile: TileId, dem: &Dem, shading: &Shading) -> Result<RasterImage, ServiceError> {
    let (nw, side) = tile_square(tile);
    let mid = crate::geo::WorldPoint::new(nw.x + side / 2.0, nw.y + side / 2.0).to_lng_lat();
    let tile_meters = side * 2.0 * std::f64::consts::PI * EARTH_RADIUS * mid.lat.to_radians().cos();
    let px = (tile_meters / f64::from(dem.width)) as f32;
    // A slope shades by how steep it is, steepened by the exaggeration,
    // and by how squarely it faces the light: lit facing it, shadowed
    // facing away, clear on the flat.
    let gain = 0.5 + 2.0 * shading.exaggeration.clamp(0.0, 1.0);
    let az = shading.illumination.to_radians();
    // Toward the light, in the tile's x east and y south.
    let light = [az.sin(), -az.cos()];
    let (shadow, highlight) = (shading.shadow, shading.highlight);
    let mut out = Vec::with_capacity(dem.meters.len() * 4);
    let to8 = |v: f32| (v.clamp(0.0, 1.0) * 255.0).round() as u8;
    for y in 0..i64::from(dem.height) {
        for x in 0..i64::from(dem.width) {
            let e = |dx: i64, dy: i64| dem.at(x + dx, y + dy);
            let dzdx = ((e(1, -1) + 2.0 * e(1, 0) + e(1, 1))
                - (e(-1, -1) + 2.0 * e(-1, 0) + e(-1, 1)))
                / (8.0 * px);
            let dzdy = ((e(-1, 1) + 2.0 * e(0, 1) + e(1, 1))
                - (e(-1, -1) + 2.0 * e(0, -1) + e(1, -1)))
                / (8.0 * px);
            let grade = dzdx.hypot(dzdy);
            let shade = if grade > 1e-6 {
                // The way the slope faces: downhill.
                let facing = (-dzdx * light[0] - dzdy * light[1]) / grade;
                (grade * gain).atan().sin() * facing
            } else {
                0.0
            };
            let (c, a) = if shade < 0.0 {
                (shadow, -shade * shadow.a)
            } else {
                (highlight, shade * highlight.a)
            };
            // The palette's channels are sRGB already.
            out.extend_from_slice(&[to8(c.r), to8(c.g), to8(c.b), to8(a)]);
        }
    }
    RasterImage::from_straight_rgba8(dem.width, dem.height, &out).map_err(|e| failed(tile, e))
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// A PNG of `w × h` opaque RGB `pixels`, stored without compression.
    pub fn png(w: u32, h: u32, rgb: &[[u8; 3]]) -> Vec<u8> {
        fn crc(bytes: &[u8]) -> u32 {
            let mut c = !0u32;
            for &b in bytes {
                c ^= u32::from(b);
                for _ in 0..8 {
                    c = (c >> 1) ^ (0xedb8_8320 & (!(c & 1)).wrapping_add(1));
                }
            }
            !c
        }
        fn chunk(out: &mut Vec<u8>, kind: &[u8], data: &[u8]) {
            out.extend_from_slice(&(data.len() as u32).to_be_bytes());
            let mut c = kind.to_vec();
            c.extend_from_slice(data);
            out.extend_from_slice(&c);
            out.extend_from_slice(&crc(&c).to_be_bytes());
        }
        let mut raw = Vec::new();
        for row in rgb.chunks(w as usize) {
            raw.push(0);
            for p in row {
                raw.extend_from_slice(p);
            }
        }
        let mut out = b"\x89PNG\r\n\x1a\n".to_vec();
        let mut ihdr = Vec::new();
        ihdr.extend_from_slice(&w.to_be_bytes());
        ihdr.extend_from_slice(&h.to_be_bytes());
        ihdr.extend_from_slice(&[8, 2, 0, 0, 0]);
        chunk(&mut out, b"IHDR", &ihdr);
        chunk(
            &mut out,
            b"IDAT",
            &miniz_oxide::deflate::compress_to_vec_zlib(&raw, 6),
        );
        chunk(&mut out, b"IEND", &[]);
        out
    }

    /// Terrarium pixels for `meters`.
    pub fn terrarium(meters: f32) -> [u8; 3] {
        let v = meters + 32768.0;
        let r = (v / 256.0).floor();
        let g = (v - r * 256.0).floor();
        let b = ((v - r * 256.0 - g) * 256.0).round();
        [r as u8, g as u8, b as u8]
    }

    #[test]
    fn elevations_read_in_both_encodings() {
        let tile = TileId::new(10, 500, 300);
        let px = [
            terrarium(0.0),
            terrarium(1234.5),
            terrarium(-50.25),
            terrarium(8848.0),
        ];
        let dem = decode_dem(tile, &png(2, 2, &px), DemEncoding::Terrarium).unwrap();
        for (got, want) in dem.meters.iter().zip([0.0, 1234.5, -50.25, 8848.0]) {
            assert!((got - want).abs() < 0.01, "{got} {want}");
        }
        // 1000 m in Mapbox RGB: (1000 + 10000) * 10 = 110000.
        let v = 110_000u32;
        let mb = [[(v >> 16) as u8, (v >> 8) as u8, v as u8]];
        let dem = decode_dem(tile, &png(1, 1, &mb), DemEncoding::MapboxRgb).unwrap();
        assert!((dem.meters[0] - 1000.0).abs() < 0.01);
        assert!(decode_dem(tile, b"nope", DemEncoding::Terrarium).is_err());
    }

    #[test]
    fn a_slope_facing_the_light_brightens_and_one_facing_away_darkens() {
        let tile = TileId::new(12, 2200, 1343);
        // Rising eastward on the west half, falling on the east half.
        let n = 16u32;
        let meters: Vec<f32> = (0..n * n)
            .map(|i| {
                let x = (i % n) as f32;
                if x < 8.0 {
                    x * 400.0
                } else {
                    (16.0 - x) * 400.0
                }
            })
            .collect();
        let dem = Dem {
            width: n,
            height: n,
            meters,
        };
        let shading = Shading {
            encoding: DemEncoding::Terrarium,
            exaggeration: 0.5,
            shadow: Color::rgba(0, 0, 0, 0.5),
            highlight: Color::rgba(255, 255, 255, 0.5),
            // Light from the west.
            illumination: 270.0,
        };
        let img = hillshade(tile, &dem, &shading).unwrap();
        let px = img.to_straight_rgba8().unwrap();
        let at = |x: usize, y: usize| &px[(y * n as usize + x) * 4..][..4];
        // Rising east, the west half faces the western light; falling east,
        // the east half faces away.
        assert!(at(3, 8)[0] > 200 && at(3, 8)[3] > 20, "{:?}", at(3, 8));
        assert!(at(12, 8)[0] < 50 && at(12, 8)[3] > 20, "{:?}", at(12, 8));
        assert!((dem.sample(0.5, 0.5) - 3000.0).abs() < 401.0);
    }
}
