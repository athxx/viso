//! WebP, lossy or lossless: a still image turned upright as its EXIF says,
//! an animated one's frames composited by the decoder onto a transparent
//! canvas — the background colour the file suggests is ignored, as browsers
//! ignore it.

use std::io::Cursor;
use std::time::Duration;

use image_webp::{DecodingError, LoopCount, WebPDecoder};
use viso_schema::resource::AssetBytes;

use super::{Orientation, exif};
use crate::animation::{Header, Loops, Piece};
use crate::raster::{AssetError, MAX_PIXELS, RasterImage, check_size, premultiplied};

fn error(e: DecodingError) -> AssetError {
    AssetError::Decode(e.to_string())
}

/// A decoder of `bytes`, its size checked.
fn decoder<R: std::io::BufRead + std::io::Seek>(
    bytes: R,
) -> Result<(WebPDecoder<R>, usize, usize), AssetError> {
    let mut decoder = WebPDecoder::new(bytes).map_err(error)?;
    decoder.set_memory_limit(MAX_PIXELS as usize * 4);
    let (w, h) = decoder.dimensions();
    let (w, h) = check_size(w as usize, h as usize)?;
    Ok((decoder, w as usize, h as usize))
}

pub(crate) fn still(bytes: &[u8]) -> Result<RasterImage, AssetError> {
    let (mut decoder, w, h) = decoder(Cursor::new(bytes))?;
    let len = decoder
        .output_buffer_size()
        .ok_or_else(|| AssetError::Decode("the WebP is too large".to_owned()))?;
    let mut pixels = vec![0; len];
    if decoder.is_animated() {
        decoder.set_background_color([0; 4]).map_err(error)?;
    }
    decoder.read_image(&mut pixels).map_err(error)?;
    let channels = if decoder.has_alpha() { 4 } else { 3 };
    let orientation = decoder
        .exif_metadata()
        .ok()
        .flatten()
        .and_then(|e| exif::orientation(&e));
    let image = premultiplied(&pixels, channels, w as u32, h as u32)?;
    Ok(Orientation::of(orientation).apply(image))
}

/// An animated WebP's frames, in order.
pub(crate) struct WebPFrames {
    decoder: WebPDecoder<Cursor<AssetBytes>>,
    alpha: bool,
    frame: Vec<u8>,
    pixels: Vec<u8>,
}

impl WebPFrames {
    /// The frames of `bytes`; `None` when the WebP is not animated.
    pub(crate) fn open(bytes: &AssetBytes) -> Result<Option<(WebPFrames, Header)>, AssetError> {
        let (mut decoder, w, h) = decoder(Cursor::new(bytes.clone()))?;
        if !decoder.is_animated() || decoder.num_frames() < 2 {
            return Ok(None);
        }
        decoder.set_background_color([0; 4]).map_err(error)?;
        let len = decoder
            .output_buffer_size()
            .ok_or_else(|| AssetError::Decode("the WebP is too large".to_owned()))?;
        let header = Header {
            width: w,
            height: h,
            loops: match decoder.loop_count() {
                LoopCount::Forever => Loops::Forever,
                LoopCount::Times(n) => Loops::plays(u32::from(n.get())),
            },
            frames: Some(decoder.num_frames()),
        };
        let alpha = decoder.has_alpha();
        Ok(Some((
            WebPFrames {
                decoder,
                alpha,
                frame: vec![0; len],
                pixels: Vec::with_capacity(w * h * 4),
            },
            header,
        )))
    }

    pub(crate) fn next(&mut self) -> Result<Option<Piece<'_>>, AssetError> {
        let ms = match self.decoder.read_frame(&mut self.frame) {
            Ok(ms) => ms,
            Err(DecodingError::NoMoreFrames) => return Ok(None),
            Err(e) => return Err(error(e)),
        };
        let pixels = if self.alpha {
            &self.frame
        } else {
            self.pixels.clear();
            for p in self.frame.as_chunks::<3>().0 {
                self.pixels.extend_from_slice(&[p[0], p[1], p[2], 255]);
            }
            &self.pixels
        };
        Ok(Some(Piece::Whole {
            pixels,
            delay: Duration::from_millis(u64::from(ms)),
        }))
    }
}
