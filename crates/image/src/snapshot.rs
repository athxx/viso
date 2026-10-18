//! Headless snapshots: the image a UI scenario's `expect snapshot` step
//! captures from the software raster, its PNG file, and how it compares with
//! the golden it is held to.
//!
//! A comparison is exact first — the same size and the same bytes pass at
//! once — then perceptual: each pixel's difference is the distance of the two
//! colors in YIQ, composited over white, as a fraction of the largest such
//! distance, so a change the eye barely sees counts for little and one in
//! brightness for more than one in hue. A pixel differs when its distance is
//! above the tolerance's threshold; the images match when no more pixels
//! differ than the tolerance allows.
//!
//! Cold path: one capture per snapshot step, one comparison per golden.

/// An image: straight-alpha RGBA8, rows top to bottom.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Image {
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
}

/// How far an image may stray from its golden.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Tolerance {
    /// The perceptual distance, `0` to `1`, above which a pixel differs.
    pub threshold: f32,
    /// How many pixels may differ.
    pub pixels: u64,
}

impl Default for Tolerance {
    /// A distance of `0.05`, no pixel: a headless render is deterministic,
    /// so only rounding is forgiven.
    fn default() -> Self {
        Tolerance {
            threshold: 0.05,
            pixels: 0,
        }
    }
}

/// How an image compares with its golden.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Comparison {
    /// Whether the sizes agree; pixels compare only when they do.
    pub same_size: bool,
    /// Whether every byte agrees.
    pub identical: bool,
    /// The pixels whose distance is above the threshold.
    pub differing: u64,
    /// The largest distance of any pixel, `0` to `1`.
    pub largest: f32,
}

impl Comparison {
    /// Whether the image matches its golden under `tolerance`.
    pub fn matches(&self, tolerance: Tolerance) -> bool {
        self.same_size && (self.identical || self.differing <= tolerance.pixels)
    }
}

impl Image {
    /// An image from tightly packed BGRA8, as a raster readback gives it.
    pub fn from_bgra8(width: u32, height: u32, bgra: &[u8]) -> Image {
        let mut rgba = bgra.to_vec();
        for pixel in rgba.as_chunks_mut::<4>().0 {
            pixel.swap(0, 2);
        }
        Image {
            width,
            height,
            rgba,
        }
    }

    /// The image as a PNG file: 8-bit RGBA, each row filtered as best
    /// compresses it, deflated.
    pub fn to_png(&self) -> Vec<u8> {
        let stride = self.width as usize * 4;
        let mut filtered = Vec::with_capacity((stride + 1) * self.height as usize);
        let zero = vec![0u8; stride];
        let mut candidate = vec![0u8; stride];
        let mut best = vec![0u8; stride];
        for y in 0..self.height as usize {
            let row = &self.rgba[y * stride..(y + 1) * stride];
            let above = if y == 0 {
                &zero[..]
            } else {
                &self.rgba[(y - 1) * stride..y * stride]
            };
            let mut best_filter = 0u8;
            let mut best_cost = u64::MAX;
            for filter in 0..5u8 {
                filter_row(filter, row, above, &mut candidate);
                let cost = candidate
                    .iter()
                    .map(|&b| u64::from((b as i8).unsigned_abs()))
                    .sum();
                if cost < best_cost {
                    best_cost = cost;
                    best_filter = filter;
                    std::mem::swap(&mut best, &mut candidate);
                }
            }
            filtered.push(best_filter);
            filtered.extend_from_slice(&best);
        }
        let data = miniz_oxide::deflate::compress_to_vec_zlib(&filtered, 8);
        let mut out = Vec::with_capacity(data.len() + 64);
        out.extend_from_slice(b"\x89PNG\r\n\x1a\n");
        let mut header = Vec::with_capacity(13);
        header.extend_from_slice(&self.width.to_be_bytes());
        header.extend_from_slice(&self.height.to_be_bytes());
        // 8 bits a channel, RGBA, deflate, adaptive filtering, no interlace.
        header.extend_from_slice(&[8, 6, 0, 0, 0]);
        chunk(&mut out, b"IHDR", &header);
        chunk(&mut out, b"IDAT", &data);
        chunk(&mut out, b"IEND", &[]);
        out
    }

    /// The image a PNG file holds, whatever its color type and depth.
    ///
    /// # Errors
    ///
    /// Why the bytes are not a PNG this reads.
    pub fn from_png(bytes: &[u8]) -> Result<Image, String> {
        use zune_core::bytestream::ZCursor;
        use zune_core::options::DecoderOptions;
        let options = DecoderOptions::default()
            .png_set_add_alpha_channel(true)
            .png_set_strip_to_8bit(true);
        let mut decoder = zune_png::PngDecoder::new_with_options(ZCursor::new(bytes), options);
        decoder
            .decode_headers()
            .map_err(|error| error.to_string())?;
        let (width, height) = decoder
            .dimensions()
            .ok_or_else(|| "the file has no PNG header".to_owned())?;
        let pixels = decoder.decode_raw().map_err(|error| error.to_string())?;
        let count = width * height;
        if count == 0 || pixels.len() % count != 0 {
            return Err("the PNG's pixels do not fill its size".to_owned());
        }
        let rgba = match pixels.len() / count {
            4 => pixels,
            2 => pixels
                .as_chunks::<2>()
                .0
                .iter()
                .flat_map(|&[v, a]| [v, v, v, a])
                .collect(),
            3 => pixels
                .as_chunks::<3>()
                .0
                .iter()
                .flat_map(|&[r, g, b]| [r, g, b, 255])
                .collect(),
            1 => pixels.iter().flat_map(|&v| [v, v, v, 255]).collect(),
            channels => return Err(format!("a PNG of {channels} channels")),
        };
        Ok(Image {
            width: width as u32,
            height: height as u32,
            rgba,
        })
    }
}

/// Compares `actual` with `expected`, a pixel differing above `threshold`.
pub fn compare(expected: &Image, actual: &Image, threshold: f32) -> Comparison {
    let same_size = expected.width == actual.width && expected.height == actual.height;
    if !same_size {
        return Comparison {
            same_size,
            identical: false,
            differing: 0,
            largest: 0.0,
        };
    }
    if expected.rgba == actual.rgba {
        return Comparison {
            same_size,
            identical: true,
            differing: 0,
            largest: 0.0,
        };
    }
    let mut differing = 0;
    let mut largest = 0f32;
    for (a, b) in expected
        .rgba
        .as_chunks::<4>()
        .0
        .iter()
        .zip(actual.rgba.as_chunks::<4>().0)
    {
        if a == b {
            continue;
        }
        let distance = distance(a, b);
        largest = largest.max(distance);
        if distance > threshold {
            differing += 1;
        }
    }
    Comparison {
        same_size,
        identical: false,
        differing,
        largest,
    }
}

/// An image of where `actual` departs from `expected`: the golden faded to
/// light gray, each pixel differing above `threshold` red, each departing
/// below it yellow, and the part one image covers and the other does not
/// magenta.
pub fn diff(expected: &Image, actual: &Image, threshold: f32) -> Image {
    let width = expected.width.max(actual.width);
    let height = expected.height.max(actual.height);
    let mut rgba = Vec::with_capacity(width as usize * height as usize * 4);
    let at = |image: &Image, x: u32, y: u32| {
        (x < image.width && y < image.height).then(|| {
            let i = (y as usize * image.width as usize + x as usize) * 4;
            [
                image.rgba[i],
                image.rgba[i + 1],
                image.rgba[i + 2],
                image.rgba[i + 3],
            ]
        })
    };
    for y in 0..height {
        for x in 0..width {
            let pixel = match (at(expected, x, y), at(actual, x, y)) {
                (Some(a), Some(b)) if a == b => {
                    let (luma, _, _) = yiq(a);
                    let faded = (255.0 + (luma - 255.0) * 0.1).round() as u8;
                    [faded, faded, faded, 255]
                }
                (Some(a), Some(b)) if distance(&a, &b) > threshold => [255, 0, 0, 255],
                (Some(_), Some(_)) => [255, 200, 0, 255],
                _ => [255, 0, 255, 255],
            };
            rgba.extend_from_slice(&pixel);
        }
    }
    Image {
        width,
        height,
        rgba,
    }
}

/// The largest squared YIQ distance: black against white.
const LARGEST: f32 = 35215.0;

/// The perceptual distance of two RGBA8 pixels, `0` to `1`.
fn distance(a: &[u8; 4], b: &[u8; 4]) -> f32 {
    let (ya, ia, qa) = yiq(*a);
    let (yb, ib, qb) = yiq(*b);
    let (dy, di, dq) = (ya - yb, ia - ib, qa - qb);
    let squared = 0.5053 * dy * dy + 0.299 * di * di + 0.1957 * dq * dq;
    (squared / LARGEST).sqrt().min(1.0)
}

/// A pixel composited over white, in YIQ.
fn yiq(pixel: [u8; 4]) -> (f32, f32, f32) {
    let alpha = f32::from(pixel[3]) / 255.0;
    let over = |c: u8| 255.0 + (f32::from(c) - 255.0) * alpha;
    let (r, g, b) = (over(pixel[0]), over(pixel[1]), over(pixel[2]));
    (
        r * 0.298_895_3 + g * 0.586_622_5 + b * 0.114_482_23,
        r * 0.595_977_99 - g * 0.274_176_1 - b * 0.321_801_9,
        r * 0.211_470_17 - g * 0.522_617_1 + b * 0.311_146_94,
    )
}

/// Row `row` under PNG filter `filter` (0 none, 1 sub, 2 up, 3 average,
/// 4 Paeth), `above` the row before it, into `out`.
fn filter_row(filter: u8, row: &[u8], above: &[u8], out: &mut [u8]) {
    const BPP: usize = 4;
    for i in 0..row.len() {
        let left = if i >= BPP { row[i - BPP] } else { 0 };
        let up = above[i];
        let corner = if i >= BPP { above[i - BPP] } else { 0 };
        let predicted = match filter {
            0 => 0,
            1 => left,
            2 => up,
            3 => ((u16::from(left) + u16::from(up)) / 2) as u8,
            _ => paeth(left, up, corner),
        };
        out[i] = row[i].wrapping_sub(predicted);
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

/// Appends a PNG chunk: length, type, data, CRC of type and data.
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
    for &byte in bytes {
        crc = TABLE[((crc ^ u32::from(byte)) & 0xFF) as usize] ^ (crc >> 8);
    }
    !crc
}

#[cfg(test)]
mod tests {
    use super::*;

    fn gradient(width: u32, height: u32) -> Image {
        let mut rgba = Vec::new();
        for y in 0..height {
            for x in 0..width {
                rgba.extend_from_slice(&[(x * 7) as u8, (y * 5) as u8, 128, 255 - x as u8]);
            }
        }
        Image {
            width,
            height,
            rgba,
        }
    }

    #[test]
    fn a_png_round_trips_and_compresses() {
        let image = gradient(64, 48);
        let png = image.to_png();
        assert!(png.starts_with(b"\x89PNG\r\n\x1a\n"));
        assert!(png.len() < image.rgba.len() / 2, "{} bytes", png.len());
        assert_eq!(Image::from_png(&png).unwrap(), image);
        let flat = Image {
            width: 300,
            height: 200,
            rgba: [40, 40, 40, 255].repeat(300 * 200),
        };
        assert!(flat.to_png().len() < 2_000);
        assert_eq!(Image::from_png(&flat.to_png()).unwrap(), flat);
        assert!(Image::from_png(b"not a png").is_err());
    }

    #[test]
    fn bgra_readback_becomes_rgba() {
        let image = Image::from_bgra8(1, 1, &[1, 2, 3, 4]);
        assert_eq!(image.rgba, [3, 2, 1, 4]);
    }

    #[test]
    fn the_crc_is_png_s() {
        assert_eq!(crc32(b"IEND"), 0xAE42_6082);
    }

    #[test]
    fn comparison_is_exact_then_perceptual() {
        let golden = gradient(16, 16);
        let same = compare(&golden, &golden.clone(), 0.05);
        assert!(same.identical && same.matches(Tolerance::default()));

        let mut rounded = golden.clone();
        rounded.rgba[0] = rounded.rgba[0].wrapping_add(1);
        let near = compare(&golden, &rounded, 0.05);
        assert!(!near.identical && near.differing == 0 && near.largest > 0.0);
        assert!(near.matches(Tolerance::default()));

        let mut changed = golden.clone();
        for pixel in changed.rgba.as_chunks_mut::<4>().0.iter_mut().take(3) {
            pixel.copy_from_slice(&[255, 0, 0, 255]);
        }
        let far = compare(&golden, &changed, 0.05);
        assert_eq!(far.differing, 3);
        assert!(!far.matches(Tolerance::default()));
        assert!(far.matches(Tolerance {
            threshold: 0.05,
            pixels: 3
        }));

        let small = gradient(8, 16);
        let sized = compare(&golden, &small, 0.05);
        assert!(!sized.same_size && !sized.matches(Tolerance::default()));
    }

    #[test]
    fn distance_is_perceptual_over_white() {
        assert!(distance(&[0, 0, 0, 255], &[255, 255, 255, 255]) > 0.95);
        let dark = distance(&[0, 0, 0, 255], &[24, 24, 24, 255]);
        let blue = distance(&[0, 0, 0, 255], &[0, 0, 24, 255]);
        assert!(dark > blue, "{dark} {blue}");
        assert_eq!(distance(&[9, 9, 9, 0], &[200, 10, 10, 0]), 0.0);
    }

    #[test]
    fn the_diff_marks_what_departs() {
        let golden = gradient(4, 2);
        let mut actual = golden.clone();
        actual.rgba[0..4].copy_from_slice(&[255, 0, 0, 255]);
        let image = diff(&golden, &actual, 0.05);
        assert_eq!(&image.rgba[0..4], &[255, 0, 0, 255]);
        let gray = &image.rgba[4..8];
        assert!(gray[0] == gray[1] && gray[1] == gray[2] && gray[0] > 200);
        let wider = diff(&golden, &gradient(5, 2), 0.05);
        assert_eq!(
            (wider.width, &wider.rgba[16..20]),
            (5, &[255, 0, 255, 255][..])
        );
    }
}
