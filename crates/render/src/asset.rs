//! Assets as the renderer draws them: a resource's identity, its format sniffed
//! from its bytes, a raster decoded to the texture convention, and a vector
//! scene placed into a box.
//!
//! A resource is named by the [`ResourceId`] of its path under the package's
//! `assets/` directory; the path exists only while compiling and packaging.
//! Every payload a resource takes on over a session — a build, an edit — has its
//! own [`ResourceRevision`].
//!
//! Decoding is bounded: a raster larger than [`MAX_DIMENSION`] on a side or
//! [`MAX_PIXELS`] in all is refused before its pixels are allocated, so hostile
//! bytes cost at most their header. Errors are values; nothing here panics on
//! input.

use std::sync::Arc;

use crate::primitive::{Path, PathCmd, Point, Rect, Rgba};

/// A resource, by its path under `assets/`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ResourceId(pub u64);

impl ResourceId {
    /// The resource at `path`, already [normalized](normalize_path).
    pub const fn of(path: &str) -> ResourceId {
        let bytes = path.as_bytes();
        let mut h: u64 = 0xcbf2_9ce4_8422_2325;
        let mut i = 0;
        while i < bytes.len() {
            h ^= bytes[i] as u64;
            h = h.wrapping_mul(0x0000_0100_0000_01b3);
            i += 1;
        }
        ResourceId(h)
    }
}

/// One payload of a resource; every new payload takes the next.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Default)]
pub struct ResourceRevision(pub u32);

/// `path` as one spelling of a file under `assets/`: `/`-separated, without
/// empty or `.` segments; `None` when it is absolute, climbs out with `..`, or
/// names nothing.
pub fn normalize_path(path: &str) -> Option<String> {
    if path.starts_with('/') || path.starts_with('\\') || path.contains(':') {
        return None;
    }
    let mut out: Vec<&str> = Vec::new();
    for segment in path.split(['/', '\\']) {
        match segment {
            "" | "." => {}
            ".." => return None,
            s => out.push(s),
        }
    }
    (!out.is_empty()).then(|| out.join("/"))
}

/// What an asset's bytes are.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum AssetFormat {
    Png,
    Jpeg,
    Svg,
}

/// How an asset draws.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum AssetKind {
    /// Pixels, sampled from a texture.
    Raster,
    /// Paths, placed into the box at any size.
    Vector,
}

impl AssetFormat {
    /// The format `bytes` are in, read from their signature: the PNG and JPEG
    /// magic numbers, an SVG root element within the document's head.
    pub fn sniff(bytes: &[u8]) -> Option<AssetFormat> {
        if bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
            return Some(AssetFormat::Png);
        }
        if bytes.starts_with(&[0xff, 0xd8, 0xff]) {
            return Some(AssetFormat::Jpeg);
        }
        let head = &bytes[..bytes.len().min(4096)];
        let text = match std::str::from_utf8(head) {
            Ok(text) => text,
            // The head may end inside a character.
            Err(e) if e.error_len().is_none() => {
                std::str::from_utf8(&head[..e.valid_up_to()]).unwrap_or_default()
            }
            Err(_) => return None,
        };
        let text = text.trim_start_matches('\u{feff}').trim_start();
        (text.starts_with('<') && text.contains("<svg")).then_some(AssetFormat::Svg)
    }

    pub const fn kind(self) -> AssetKind {
        match self {
            AssetFormat::Png | AssetFormat::Jpeg => AssetKind::Raster,
            AssetFormat::Svg => AssetKind::Vector,
        }
    }

    /// Its common name, for messages.
    pub const fn name(self) -> &'static str {
        match self {
            AssetFormat::Png => "PNG",
            AssetFormat::Jpeg => "JPEG",
            AssetFormat::Svg => "SVG",
        }
    }
}

/// The longest side a raster may have.
pub const MAX_DIMENSION: u32 = 16_384;
/// The most pixels a raster may have: 64 megapixels, 256 MiB decoded.
pub const MAX_PIXELS: u64 = 1 << 26;

/// Why an asset did not decode.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AssetError {
    /// The bytes are in no format the asset system reads.
    Format,
    /// The bytes are in a format another kind of asset uses.
    Kind(AssetFormat),
    /// The decoder refused the bytes.
    Decode(String),
    /// The raster exceeds [`MAX_DIMENSION`] or [`MAX_PIXELS`].
    TooLarge { width: u32, height: u32 },
}

impl std::fmt::Display for AssetError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            AssetError::Format => f.write_str("not a PNG, JPEG or SVG file"),
            AssetError::Kind(format) => write!(f, "a {} file draws another way", format.name()),
            AssetError::Decode(why) => f.write_str(why),
            AssetError::TooLarge { width, height } => write!(
                f,
                "a {width}×{height} image is larger than {MAX_DIMENSION} pixels on a side or \
                 {MAX_PIXELS} in all"
            ),
        }
    }
}

impl std::error::Error for AssetError {}

/// A decoded raster in the texture convention: RGBA8, premultiplied, the
/// sRGB-encoded channels as stored.
#[derive(Clone, PartialEq, Eq)]
pub struct RasterImage {
    pub width: u32,
    pub height: u32,
    /// `width × height × 4` bytes, row by row from the top.
    pub pixels: Vec<u8>,
}

impl std::fmt::Debug for RasterImage {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "RasterImage({}×{})", self.width, self.height)
    }
}

impl RasterImage {
    /// Its natural size in pixels.
    pub fn size(&self) -> [f32; 2] {
        [self.width as f32, self.height as f32]
    }
}

fn check_size(width: usize, height: usize) -> Result<(u32, u32), AssetError> {
    let (w, h) = (
        u32::try_from(width).unwrap_or(u32::MAX),
        u32::try_from(height).unwrap_or(u32::MAX),
    );
    if w == 0 || h == 0 {
        return Err(AssetError::Decode("the image has no pixels".to_owned()));
    }
    if w > MAX_DIMENSION || h > MAX_DIMENSION || u64::from(w) * u64::from(h) > MAX_PIXELS {
        return Err(AssetError::TooLarge {
            width: w,
            height: h,
        });
    }
    Ok((w, h))
}

/// `pixels` of `channels` channels each (gray, gray-alpha, RGB or RGBA) as
/// premultiplied RGBA8.
fn premultiplied(
    pixels: &[u8],
    channels: usize,
    w: u32,
    h: u32,
) -> Result<RasterImage, AssetError> {
    let count = w as usize * h as usize;
    if channels == 0 || pixels.len() != count * channels {
        return Err(AssetError::Decode(
            "the decoder gave pixels of an unexpected shape".to_owned(),
        ));
    }
    let mut rgba = Vec::with_capacity(count * 4);
    for p in pixels.chunks_exact(channels) {
        let (r, g, b, a) = match channels {
            4 => (p[0], p[1], p[2], p[3]),
            3 => (p[0], p[1], p[2], 255),
            2 => (p[0], p[0], p[0], p[1]),
            1 => (p[0], p[0], p[0], 255),
            _ => {
                return Err(AssetError::Decode(format!("{channels} channels per pixel")));
            }
        };
        let premul = |v: u8| ((u16::from(v) * u16::from(a) + 127) / 255) as u8;
        rgba.extend_from_slice(&[premul(r), premul(g), premul(b), a]);
    }
    Ok(RasterImage {
        width: w,
        height: h,
        pixels: rgba,
    })
}

/// Decodes a PNG or JPEG.
///
/// # Errors
///
/// Bytes in no raster format, a decoder error, or a raster past the limits —
/// checked from the header, before any pixel is allocated.
pub fn decode_raster(bytes: &[u8]) -> Result<RasterImage, AssetError> {
    use zune_core::bytestream::ZCursor;
    use zune_core::options::DecoderOptions;
    let error = |e: &dyn std::fmt::Display| AssetError::Decode(e.to_string());
    match AssetFormat::sniff(bytes) {
        Some(AssetFormat::Png) => {
            let options = DecoderOptions::default()
                .png_set_add_alpha_channel(true)
                .png_set_strip_to_8bit(true)
                .set_max_width(MAX_DIMENSION as usize)
                .set_max_height(MAX_DIMENSION as usize);
            let mut decoder = zune_png::PngDecoder::new_with_options(ZCursor::new(bytes), options);
            decoder.decode_headers().map_err(|e| error(&e))?;
            let (w, h) = decoder
                .dimensions()
                .ok_or_else(|| AssetError::Decode("the PNG has no header".to_owned()))?;
            let (w, h) = check_size(w, h)?;
            let pixels = decoder.decode_raw().map_err(|e| error(&e))?;
            let channels = pixels.len() / (w as usize * h as usize);
            premultiplied(&pixels, channels, w, h)
        }
        Some(AssetFormat::Jpeg) => {
            let options = DecoderOptions::default()
                .jpeg_set_out_colorspace(zune_core::colorspace::ColorSpace::RGB)
                .set_max_width(MAX_DIMENSION as usize)
                .set_max_height(MAX_DIMENSION as usize);
            let mut decoder =
                zune_jpeg::JpegDecoder::new_with_options(ZCursor::new(bytes), options);
            decoder.decode_headers().map_err(|e| error(&e))?;
            let (w, h) = decoder
                .dimensions()
                .ok_or_else(|| AssetError::Decode("the JPEG has no header".to_owned()))?;
            let (w, h) = check_size(w, h)?;
            let pixels = decoder.decode().map_err(|e| error(&e))?;
            premultiplied(&pixels, 3, w, h)
        }
        Some(format) => Err(AssetError::Kind(format)),
        None => Err(AssetError::Format),
    }
}

/// A vector asset: filled and stroked paths in its own space, `(0, 0)` its
/// top-left and `size` its natural extent.
#[derive(Debug, Clone, PartialEq)]
pub struct VectorScene {
    pub paths: Arc<[Path]>,
    pub size: [f32; 2],
}

impl VectorScene {
    /// Its paths placed into `rect`, stretched to fill it, each color multiplied
    /// by `tint` when given (white keeps the colors, a color with full alpha
    /// recolors a monochrome icon by its alpha).
    pub fn place<'s>(&'s self, rect: Rect, tint: Option<Rgba>) -> impl Iterator<Item = Path> + 's {
        let sx = if self.size[0] > 0.0 {
            rect.w / self.size[0]
        } else {
            0.0
        };
        let sy = if self.size[1] > 0.0 {
            rect.h / self.size[1]
        } else {
            0.0
        };
        let at = move |p: Point| Point {
            x: rect.x + p.x * sx,
            y: rect.y + p.y * sy,
        };
        // A stroke scales by the geometric mean, exact for a uniform scale.
        let width_scale = (sx * sy).abs().sqrt();
        let tinted = move |c: Rgba| match tint {
            Some(t) => Rgba {
                r: c.r * t.r,
                g: c.g * t.g,
                b: c.b * t.b,
                a: c.a * t.a,
            },
            None => c,
        };
        self.paths.iter().map(move |path| Path {
            cmds: path
                .cmds
                .iter()
                .map(|cmd| match *cmd {
                    PathCmd::MoveTo(p) => PathCmd::MoveTo(at(p)),
                    PathCmd::LineTo(p) => PathCmd::LineTo(at(p)),
                    PathCmd::QuadTo(c, p) => PathCmd::QuadTo(at(c), at(p)),
                    PathCmd::CubicTo(a, b, p) => PathCmd::CubicTo(at(a), at(b), at(p)),
                    PathCmd::Close => PathCmd::Close,
                })
                .collect(),
            fill: path.fill.map(tinted),
            stroke: path.stroke.map(|mut s| {
                s.width *= width_scale;
                s.color = tinted(s.color);
                if let Some(dash) = &mut s.dash {
                    for seg in &mut dash.segments {
                        *seg *= width_scale;
                    }
                    dash.offset *= width_scale;
                }
                s
            }),
            shadow: path.shadow,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_path_normalizes_to_one_spelling_and_never_leaves_assets() {
        assert_eq!(
            normalize_path("images/./icons//a.png").as_deref(),
            Some("images/icons/a.png")
        );
        assert_eq!(
            normalize_path("images\\a.png").as_deref(),
            Some("images/a.png")
        );
        for bad in [
            "/etc/passwd",
            "../secret.png",
            "a/../../b",
            "",
            "./",
            "C:/x.png",
        ] {
            assert_eq!(normalize_path(bad), None, "{bad}");
        }
        assert_eq!(
            ResourceId::of("images/a.png"),
            ResourceId::of(&normalize_path("images//a.png").unwrap())
        );
        assert_ne!(ResourceId::of("a.png"), ResourceId::of("b.png"));
    }

    #[test]
    fn the_format_is_read_from_the_bytes() {
        assert_eq!(
            AssetFormat::sniff(b"\x89PNG\r\n\x1a\n...."),
            Some(AssetFormat::Png)
        );
        assert_eq!(
            AssetFormat::sniff(&[0xff, 0xd8, 0xff, 0xe0]),
            Some(AssetFormat::Jpeg)
        );
        assert_eq!(
            AssetFormat::sniff(b"\xef\xbb\xbf<?xml version=\"1.0\"?>\n<svg width=\"1\"/>"),
            Some(AssetFormat::Svg)
        );
        assert_eq!(AssetFormat::sniff(b"<html><body/>"), None);
        assert_eq!(AssetFormat::sniff(b"GIF89a"), None);
        assert_eq!(decode_raster(b"GIF89a"), Err(AssetError::Format));
        assert_eq!(
            decode_raster(b"<svg/>"),
            Err(AssetError::Kind(AssetFormat::Svg))
        );
    }

    /// A PNG of `w × h` RGBA pixels, stored uncompressed.
    pub(crate) fn png(w: u32, h: u32, rgba: &[u8]) -> Vec<u8> {
        fn crc(bytes: &[u8]) -> u32 {
            let mut c = 0xffff_ffffu32;
            for &b in bytes {
                c ^= u32::from(b);
                for _ in 0..8 {
                    c = if c & 1 != 0 {
                        0xedb8_8320 ^ (c >> 1)
                    } else {
                        c >> 1
                    };
                }
            }
            !c
        }
        fn chunk(out: &mut Vec<u8>, kind: &[u8; 4], data: &[u8]) {
            out.extend_from_slice(&(data.len() as u32).to_be_bytes());
            let start = out.len();
            out.extend_from_slice(kind);
            out.extend_from_slice(data);
            let sum = crc(&out[start..]);
            out.extend_from_slice(&sum.to_be_bytes());
        }
        let mut raw = Vec::new();
        for row in rgba.chunks_exact(w as usize * 4) {
            raw.push(0);
            raw.extend_from_slice(row);
        }
        // A zlib stream of stored deflate blocks.
        let mut z = vec![0x78, 0x01];
        let blocks: Vec<&[u8]> = raw.chunks(65_535).collect();
        for (i, block) in blocks.iter().enumerate() {
            z.push(u8::from(i + 1 == blocks.len()));
            let n = block.len() as u16;
            z.extend_from_slice(&n.to_le_bytes());
            z.extend_from_slice(&(!n).to_le_bytes());
            z.extend_from_slice(block);
        }
        let (mut a, mut b) = (1u32, 0u32);
        for &byte in &raw {
            a = (a + u32::from(byte)) % 65_521;
            b = (b + a) % 65_521;
        }
        z.extend_from_slice(&((b << 16) | a).to_be_bytes());
        let mut out = b"\x89PNG\r\n\x1a\n".to_vec();
        let mut ihdr = Vec::new();
        ihdr.extend_from_slice(&w.to_be_bytes());
        ihdr.extend_from_slice(&h.to_be_bytes());
        ihdr.extend_from_slice(&[8, 6, 0, 0, 0]);
        chunk(&mut out, b"IHDR", &ihdr);
        chunk(&mut out, b"IDAT", &z);
        chunk(&mut out, b"IEND", &[]);
        out
    }

    #[test]
    fn a_png_decodes_premultiplied_and_a_bad_one_is_an_error() {
        let pixels = [
            255, 0, 0, 255, 0, 255, 0, 128, 0, 0, 255, 0, 10, 20, 30, 255,
        ];
        let image = decode_raster(&png(2, 2, &pixels)).expect("decodes");
        assert_eq!((image.width, image.height), (2, 2));
        assert_eq!(
            image.pixels,
            [255, 0, 0, 255, 0, 128, 0, 128, 0, 0, 0, 0, 10, 20, 30, 255]
        );
        let bytes = png(2, 2, &pixels);
        for end in [8, 20, 40, bytes.len() - 13] {
            assert!(decode_raster(&bytes[..end]).is_err(), "{end}");
        }
        let mut huge = png(1, 1, &[0; 4]);
        // IHDR width at byte 16: claim a raster past the limit.
        huge[16..20].copy_from_slice(&(MAX_DIMENSION + 1).to_be_bytes());
        assert!(decode_raster(&huge).is_err());
    }

    #[test]
    fn a_jpeg_decodes_opaque() {
        let image = decode_raster(include_bytes!("fixtures/two_tone.jpg")).expect("decodes");
        assert_eq!((image.width, image.height), (8, 4));
        let at = |x: usize, y: usize| {
            let i = (y * 8 + x) * 4;
            [
                image.pixels[i],
                image.pixels[i + 1],
                image.pixels[i + 2],
                image.pixels[i + 3],
            ]
        };
        let [r, _, b, a] = at(1, 1);
        assert!(r > 150 && b < 100 && a == 255, "{:?}", at(1, 1));
        let [r, _, b, a] = at(6, 2);
        assert!(b > 150 && r < 100 && a == 255, "{:?}", at(6, 2));
    }

    #[test]
    fn a_vector_scene_stretches_into_its_box_and_takes_a_tint() {
        let path = Path {
            cmds: vec![
                PathCmd::MoveTo(Point { x: 0.0, y: 0.0 }),
                PathCmd::LineTo(Point { x: 10.0, y: 20.0 }),
                PathCmd::Close,
            ],
            fill: Some(Rgba {
                r: 1.0,
                g: 0.5,
                b: 0.0,
                a: 1.0,
            }),
            stroke: Some(crate::primitive::Stroke::new(
                2.0,
                Rgba {
                    r: 0.0,
                    g: 0.0,
                    b: 0.0,
                    a: 1.0,
                },
            )),
            shadow: None,
        };
        let scene = VectorScene {
            paths: Arc::from(vec![path]),
            size: [10.0, 20.0],
        };
        let rect = Rect {
            x: 100.0,
            y: 50.0,
            w: 40.0,
            h: 80.0,
        };
        let tint = Rgba {
            r: 0.0,
            g: 0.0,
            b: 1.0,
            a: 0.5,
        };
        let placed: Vec<Path> = scene.place(rect, Some(tint)).collect();
        assert_eq!(
            placed[0].cmds[1],
            PathCmd::LineTo(Point { x: 140.0, y: 130.0 })
        );
        assert_eq!(placed[0].stroke.unwrap().width, 8.0);
        assert_eq!(
            placed[0].fill,
            Some(Rgba {
                r: 0.0,
                g: 0.0,
                b: 0.0,
                a: 0.5
            })
        );
    }
}
