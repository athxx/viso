//! PNG, and APNG: an animated PNG's frames are regions of the canvas, each
//! drawn over or in place of it and disposed of as its `fcTL` says. A still
//! PNG, or the default image of an animated one, is turned upright as its
//! `eXIf` says.

use std::time::Duration;

use viso_schema::resource::AssetBytes;
use zune_core::bytestream::ZCursor;
use zune_png::{BlendOp, DisposeOp, PngDecoder};

use super::{Orientation, error, exif, options};
use crate::animation::{Blend, Dispose, Header, Loops, Piece};
use crate::raster::{AssetError, RasterImage, check_size, premultiplied};

/// How a PNG decodes: eight bits a channel, with an alpha channel.
fn png_options() -> zune_core::options::DecoderOptions {
    options()
        .png_set_add_alpha_channel(true)
        .png_set_strip_to_8bit(true)
}

pub(crate) fn still(bytes: &[u8]) -> Result<RasterImage, AssetError> {
    let mut decoder = PngDecoder::new_with_options(ZCursor::new(bytes), png_options());
    decoder.decode_headers().map_err(|e| error(&e))?;
    let (w, h) = decoder
        .dimensions()
        .ok_or_else(|| AssetError::Decode("the PNG has no header".to_owned()))?;
    let (w, h) = check_size(w, h)?;
    let pixels = decoder.decode_raw().map_err(|e| error(&e))?;
    let channels = pixels.len() / (w as usize * h as usize);
    let orientation = decoder
        .info()
        .and_then(|info| info.exif.as_deref())
        .and_then(exif::orientation);
    Ok(Orientation::of(orientation).apply(premultiplied(&pixels, channels, w, h)?))
}

/// The frame count and plays an APNG's `acTL` declares, read from the
/// chunks before its image data; `None` for a still PNG.
fn actl(bytes: &[u8]) -> Option<(u32, u32)> {
    let mut at = 8;
    while let Some(head) = bytes.get(at..at + 8) {
        let len = u32::from_be_bytes([head[0], head[1], head[2], head[3]]) as usize;
        match &head[4..8] {
            b"acTL" => {
                let data = bytes.get(at + 8..at + 16)?;
                let frames = u32::from_be_bytes([data[0], data[1], data[2], data[3]]);
                let plays = u32::from_be_bytes([data[4], data[5], data[6], data[7]]);
                return Some((frames, plays));
            }
            b"IDAT" | b"IEND" => return None,
            _ => at = at.checked_add(12 + len)?,
        }
    }
    None
}

/// An APNG's frames, in order.
pub(crate) struct PngFrames {
    decoder: PngDecoder<ZCursor<AssetBytes>>,
    /// The frames left of those `acTL` declares: a stream with more is cut
    /// there.
    left: u32,
    pixels: Vec<u8>,
}

/// The frames of `bytes`, a PNG; `None` when it is not animated.
pub(crate) fn apng_frames(bytes: &AssetBytes) -> Result<Option<(PngFrames, Header)>, AssetError> {
    let Some((frames, plays)) = actl(bytes) else {
        return Ok(None);
    };
    if frames < 2 {
        return Ok(None);
    }
    let mut decoder = PngDecoder::new_with_options(ZCursor::new(bytes.clone()), png_options());
    decoder.decode_headers().map_err(|e| error(&e))?;
    let (w, h) = decoder
        .dimensions()
        .ok_or_else(|| AssetError::Decode("the PNG has no header".to_owned()))?;
    check_size(w, h)?;
    let header = Header {
        width: w,
        height: h,
        loops: Loops::plays(plays),
        frames: Some(frames),
    };
    Ok(Some((
        PngFrames {
            decoder,
            left: frames,
            pixels: Vec::new(),
        },
        header,
    )))
}

impl PngFrames {
    pub(crate) fn next(&mut self) -> Result<Option<Piece<'_>>, AssetError> {
        let decoder = &mut self.decoder;
        loop {
            if self.left == 0 || !decoder.more_frames() {
                return Ok(None);
            }
            decoder.decode_headers().map_err(|e| error(&e))?;
            let info = decoder
                .frame_info()
                .ok_or_else(|| AssetError::Decode("an APNG frame without an fcTL".to_owned()))?;
            let raw = decoder.decode_raw().map_err(|e| error(&e))?;
            // A default image outside the animation is for viewers without
            // it.
            if !info.is_part_of_seq {
                continue;
            }
            self.left -= 1;
            let count = info.width * info.height;
            if count == 0 || raw.len() % count != 0 {
                return Err(AssetError::Decode(
                    "an APNG frame's texels are not its size".to_owned(),
                ));
            }
            rgba(&raw, raw.len() / count, &mut self.pixels)?;
            let delay = match (info.delay_num, info.delay_denom) {
                (n, 0) => Duration::from_millis(u64::from(n) * 10),
                (n, d) => Duration::from_secs_f64(f64::from(n) / f64::from(d)),
            };
            return Ok(Some(Piece::Region {
                x: info.x_offset as u32,
                y: info.y_offset as u32,
                w: info.width as u32,
                h: info.height as u32,
                pixels: &self.pixels,
                blend: match info.blend_op {
                    BlendOp::Source => Blend::Replace,
                    BlendOp::Over => Blend::Over,
                },
                dispose: match info.dispose_op {
                    DisposeOp::None => Dispose::Keep,
                    DisposeOp::Background => Dispose::Clear,
                    DisposeOp::Previous => Dispose::Restore,
                },
                delay,
            }));
        }
    }
}

/// `pixels` of `channels` channels each as straight RGBA8 into `out`.
fn rgba(pixels: &[u8], channels: usize, out: &mut Vec<u8>) -> Result<(), AssetError> {
    out.clear();
    out.reserve(pixels.len() / channels * 4);
    for p in pixels.chunks_exact(channels) {
        out.extend_from_slice(&match channels {
            4 => [p[0], p[1], p[2], p[3]],
            3 => [p[0], p[1], p[2], 255],
            2 => [p[0], p[0], p[0], p[1]],
            1 => [p[0], p[0], p[0], 255],
            _ => return Err(AssetError::Decode(format!("{channels} channels per pixel"))),
        });
    }
    Ok(())
}
