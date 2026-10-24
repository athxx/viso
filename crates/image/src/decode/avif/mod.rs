//! AVIF: AV1 images in an ISO media container.
//!
//! A still image is its primary item — or a grid of tiles composed into one
//! — with its alpha item, in the colour its `colr` box declares (else its
//! AV1 sequence header's), cropped to its clean aperture, then turned and
//! mirrored as its `irot` and `imir` say. An image sequence plays its
//! colour track's samples, with its alpha track's, through one decoder each,
//! as stored.
//!
//! An alpha item that is itself a grid is a layout, not AV1 data: such an
//! image shows opaque.

mod av1;
mod color;

use std::borrow::Cow;
use std::time::Duration;

use viso_schema::resource::AssetBytes;
use zenavif_parse::{AvifParser, ColorInformation, DecodeConfig, Unstoppable};

use self::av1::{Av1, Signal};
use super::Orientation;
use crate::animation::{Header, Loops, Piece};
use crate::raster::{AssetError, MAX_PIXELS, RasterImage, check_size, premultiplied};

fn error(e: zenavif_parse::Error) -> AssetError {
    AssetError::Decode(e.to_string())
}

/// The container limits: the raster limit for a grid, its frames and tiles
/// bounded.
fn config() -> DecodeConfig {
    DecodeConfig::default()
        .with_peak_memory_limit(MAX_PIXELS * 8)
        .with_total_megapixels_limit((MAX_PIXELS / 1_000_000) as u32)
        .with_max_animation_frames(10_000)
        .with_max_grid_tiles(1_024)
}

/// The threads a decode of `pixels` runs: one for a small image, up to
/// eight for a large one.
fn threads(pixels: u64) -> usize {
    if pixels < 1 << 20 {
        return 1;
    }
    std::thread::available_parallelism().map_or(1, |n| n.get().min(8))
}

/// The signal an image is shown by: its `colr` box's, where it is one,
/// else its sequence header's, else sRGB's.
fn signal(parser: &AvifParser<'_>, coded: Option<Signal>) -> Signal {
    match parser.color_info() {
        Some(ColorInformation::Nclx {
            color_primaries,
            transfer_characteristics,
            matrix_coefficients,
            full_range,
        }) => Signal {
            primaries: *color_primaries,
            transfer: *transfer_characteristics,
            matrix: *matrix_coefficients,
            full: *full_range,
        },
        // An ICC profile is not read: the samples show as sRGB.
        _ => coded.unwrap_or(Signal {
            primaries: 1,
            transfer: 13,
            matrix: 6,
            full: true,
        }),
    }
}

/// The brightest the content reaches, in nits, when it records it.
fn peak(parser: &AvifParser<'_>) -> Option<f32> {
    parser
        .content_light_level()
        .map(|l| f32::from(l.max_content_light_level))
        .filter(|&nits| nits > 0.0)
}

/// Decodes `data`, an AV1 image, to straight RGBA8 with its size, with
/// `alpha`, an AV1 image of the same size, when given.
fn picture(
    parser: &AvifParser<'_>,
    data: &[u8],
    alpha: Option<&[u8]>,
) -> Result<(Vec<u8>, usize, usize), AssetError> {
    let meta = zenavif_parse::AV1Metadata::parse_av1_bitstream(data).map_err(error)?;
    let (mw, mh) = (meta.max_frame_width.get(), meta.max_frame_height.get());
    check_size(mw as usize, mh as usize)?;
    let threads = threads(u64::from(mw) * u64::from(mh));
    let alpha = match alpha {
        Some(alpha) => {
            let mut decoder = Av1::new(threads)?;
            Some(color::alpha_bytes(&decoder.decode(alpha)?)?)
        }
        None => None,
    };
    let mut decoder = Av1::new(threads)?;
    let color = decoder.decode(data)?;
    let (w, h) = color.size();
    let signal = signal(parser, color.signal());
    let rgba = color::to_rgba(
        &color,
        alpha.as_deref(),
        signal,
        parser.premultiplied_alpha(),
        peak(parser),
    )?;
    Ok((rgba, w, h))
}

pub(crate) fn still(bytes: &[u8]) -> Result<RasterImage, AssetError> {
    let parser =
        AvifParser::from_bytes_with_config(bytes, &config(), &Unstoppable).map_err(error)?;
    let (rgba, w, h) = match parser.grid_config() {
        Some(_) => grid(&parser)?,
        None => {
            let primary = match parser.primary_data() {
                Ok(data) => data,
                // A sequence with no image item shows its first sample.
                Err(e) => match parser.animation_info() {
                    Some(_) => parser.frame(0).map_err(error)?.data,
                    None => return Err(error(e)),
                },
            };
            let alpha = parser.alpha_data().transpose().map_err(error)?;
            picture(&parser, &primary, alpha.as_deref())?
        }
    };
    let (rgba, w, h) = crop(&parser, rgba, w, h);
    let (w, h) = check_size(w, h)?;
    let mut image = premultiplied(&rgba, 4, w, h)?;
    if let Some(turn) = parser.rotation() {
        image = match turn.angle {
            90 => Orientation::ANTICLOCKWISE,
            180 => Orientation::HALF_TURN,
            270 => Orientation::CLOCKWISE,
            _ => Orientation::UPRIGHT,
        }
        .apply(image);
    }
    if let Some(mirror) = parser.mirror() {
        image = match mirror.axis {
            0 => Orientation::MIRROR_ACROSS,
            _ => Orientation::MIRROR_DOWN,
        }
        .apply(image);
    }
    Ok(image)
}

/// A grid image's tiles composed, left to right then top to bottom, into
/// its output size; with its alpha where that decodes as one image.
fn grid(parser: &AvifParser<'_>) -> Result<(Vec<u8>, usize, usize), AssetError> {
    let grid = parser
        .grid_config()
        .ok_or_else(|| AssetError::Decode("not a grid".to_owned()))?;
    let (rows, columns) = (usize::from(grid.rows), usize::from(grid.columns));
    if rows * columns != parser.grid_tile_count() || rows * columns == 0 {
        return Err(AssetError::Decode(
            "the AVIF grid's tiles do not fill it".to_owned(),
        ));
    }
    let mut canvas = Vec::new();
    let (mut w, mut h) = (0, 0);
    for i in 0..rows * columns {
        let data = parser.tile_data(i).map_err(error)?;
        let (tile, tw, th) = picture(parser, &data, None)?;
        if i == 0 {
            w = match grid.output_width {
                0 => tw * columns,
                ow => ow as usize,
            };
            h = match grid.output_height {
                0 => th * rows,
                oh => oh as usize,
            };
            let (cw, ch) = check_size(w, h)?;
            canvas = vec![0u8; cw as usize * ch as usize * 4];
        }
        let (x0, y0) = ((i % columns) * tw, (i / columns) * th);
        for y in 0..th.min(h.saturating_sub(y0)) {
            let n = tw.min(w.saturating_sub(x0)) * 4;
            let at = ((y0 + y) * w + x0) * 4;
            canvas[at..at + n].copy_from_slice(&tile[y * tw * 4..][..n]);
        }
    }
    let alpha = parser.alpha_data().and_then(Result::ok).and_then(|data| {
        let mut decoder = Av1::new(threads((w * h) as u64)).ok()?;
        let picture = decoder.decode(&data).ok()?;
        (picture.size() == (w, h)).then(|| color::alpha_bytes(&picture).ok())?
    });
    if let Some(alpha) = alpha {
        for (px, a) in canvas.as_chunks_mut::<4>().0.iter_mut().zip(alpha) {
            px[3] = a;
        }
    }
    Ok((canvas, w, h))
}

/// The image cropped to its clean aperture, centred where it says; an
/// aperture outside the image is ignored.
fn crop(parser: &AvifParser<'_>, rgba: Vec<u8>, w: usize, h: usize) -> (Vec<u8>, usize, usize) {
    let Some(clap) = parser.clean_aperture() else {
        return (rgba, w, h);
    };
    let ratio = |n: i64, d: u32| (d != 0).then(|| n as f64 / f64::from(d));
    let (Some(cw), Some(ch), Some(dx), Some(dy)) = (
        ratio(clap.width_n.into(), clap.width_d),
        ratio(clap.height_n.into(), clap.height_d),
        ratio(clap.horiz_off_n.into(), clap.horiz_off_d),
        ratio(clap.vert_off_n.into(), clap.vert_off_d),
    ) else {
        return (rgba, w, h);
    };
    let x0 = ((w as f64 - cw) / 2.0 + dx).round();
    let y0 = ((h as f64 - ch) / 2.0 + dy).round();
    let (cw, ch) = (cw.round(), ch.round());
    if x0 < 0.0 || y0 < 0.0 || cw < 1.0 || ch < 1.0 || x0 + cw > w as f64 || y0 + ch > h as f64 {
        return (rgba, w, h);
    }
    let (x0, y0, cw, ch) = (x0 as usize, y0 as usize, cw as usize, ch as usize);
    if (x0, y0, cw, ch) == (0, 0, w, h) {
        return (rgba, w, h);
    }
    let mut out = Vec::with_capacity(cw * ch * 4);
    for y in y0..y0 + ch {
        out.extend_from_slice(&rgba[(y * w + x0) * 4..(y * w + x0 + cw) * 4]);
    }
    (out, cw, ch)
}

/// An AVIF image sequence's frames, in order.
pub(crate) struct AvifFrames {
    parser: AvifParser<'static>,
    color: Av1,
    alpha: Option<Av1>,
    next: usize,
    count: usize,
    size: (usize, usize),
    pixels: Vec<u8>,
}

impl AvifFrames {
    /// The frames of `bytes`; `None` when they hold no sequence of two
    /// frames or more.
    pub(crate) fn open(bytes: &AssetBytes) -> Result<Option<(AvifFrames, Header)>, AssetError> {
        let parser = match bytes {
            AssetBytes::Static(bytes) => {
                AvifParser::from_bytes_with_config(bytes, &config(), &Unstoppable)
            }
            AssetBytes::Shared(bytes) => {
                AvifParser::from_owned_with_config(bytes.to_vec(), &config(), &Unstoppable)
            }
        }
        .map_err(error)?;
        let Some(info) = parser.animation_info().filter(|i| i.frame_count >= 2) else {
            return Ok(None);
        };
        let first = parser.frame(0).map_err(error)?;
        let meta = zenavif_parse::AV1Metadata::parse_av1_bitstream(&first.data).map_err(error)?;
        let (w, h) = (
            meta.max_frame_width.get() as usize,
            meta.max_frame_height.get() as usize,
        );
        check_size(w, h)?;
        let threads = threads((w * h) as u64);
        let alpha = first.alpha_data.is_some();
        let header = Header {
            width: w,
            height: h,
            loops: Loops::plays(info.loop_count),
            frames: u32::try_from(info.frame_count).ok(),
        };
        Ok(Some((
            AvifFrames {
                parser,
                color: Av1::new(threads)?,
                alpha: alpha.then(|| Av1::new(threads)).transpose()?,
                next: 0,
                count: info.frame_count,
                size: (w, h),
                pixels: Vec::new(),
            },
            header,
        )))
    }

    pub(crate) fn next(&mut self) -> Result<Option<Piece<'_>>, AssetError> {
        if self.next == self.count {
            return Ok(None);
        }
        let frame = self.parser.frame(self.next).map_err(error)?;
        self.next += 1;
        let alpha = match (&mut self.alpha, frame.alpha_data.as_ref().map(Cow::as_ref)) {
            (Some(decoder), Some(data)) => Some(color::alpha_bytes(&decoder.decode(data)?)?),
            _ => None,
        };
        let picture = self.color.decode(&frame.data)?;
        if picture.size() != self.size {
            return Err(AssetError::Decode(
                "an AVIF frame is not the sequence's size".to_owned(),
            ));
        }
        let signal = signal(&self.parser, picture.signal());
        self.pixels = color::to_rgba(
            &picture,
            alpha.as_deref(),
            signal,
            self.parser.premultiplied_alpha(),
            peak(&self.parser),
        )?;
        Ok(Some(Piece::Whole {
            pixels: &self.pixels,
            delay: Duration::from_millis(u64::from(frame.duration_ms)),
        }))
    }
}
