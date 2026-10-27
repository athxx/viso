//! GIF: each frame a region of the logical screen, its transparent index
//! keyed out, disposed of as its graphic control extension says.

use std::io::Cursor;
use std::time::Duration;

use gif::{ColorOutput, DecodeOptions, DisposalMethod, MemoryLimit, Repeat};
use viso_schema::resource::AssetBytes;

use crate::animation::{Animation, Blend, Dispose, Frames, Header, Loops, Piece};
use crate::raster::{AssetError, MAX_PIXELS, RasterImage, check_size};

/// A GIF's frames, in order.
pub(crate) struct GifFrames {
    decoder: gif::Decoder<Cursor<AssetBytes>>,
    /// The first frame's header, read with the file's, waiting to be drawn.
    ahead: bool,
    pixels: Vec<u8>,
}

fn error(e: gif::DecodingError) -> AssetError {
    AssetError::Decode(e.to_string())
}

impl GifFrames {
    pub(crate) fn open(bytes: &AssetBytes) -> Result<(GifFrames, Header), AssetError> {
        let mut options = DecodeOptions::new();
        options.set_color_output(ColorOutput::RGBA);
        options.set_memory_limit(MemoryLimit::Bytes(
            std::num::NonZeroU64::new(MAX_PIXELS * 4).expect("nonzero"),
        ));
        let mut decoder = options
            .read_info(Cursor::new(bytes.clone()))
            .map_err(error)?;
        check_size(decoder.width().into(), decoder.height().into())?;
        // The loop count sits in an extension before the first frame.
        let ahead = decoder.next_frame_info().map_err(error)?.is_some();
        // No loop extension plays once; one counting `n` repeats `n` times.
        let loops = match decoder.repeat() {
            Repeat::Infinite => Loops::Forever,
            Repeat::Finite(n) => Loops::plays(u32::from(n) + 1),
        };
        let frames = if !ahead {
            Some(0)
        } else if Self::second(bytes) {
            None
        } else {
            Some(1)
        };
        let header = Header {
            width: decoder.width().into(),
            height: decoder.height().into(),
            loops,
            frames,
        };
        Ok((
            GifFrames {
                decoder,
                ahead,
                pixels: Vec::new(),
            },
            header,
        ))
    }

    /// Whether `bytes` hold a second frame, walked to without decoding.
    fn second(bytes: &AssetBytes) -> bool {
        let mut options = DecodeOptions::new();
        options.skip_frame_decoding(true);
        let Ok(mut decoder) = options.read_info(Cursor::new(bytes.clone())) else {
            return false;
        };
        let mut seen = 0;
        while let Ok(Some(_)) = decoder.read_next_frame() {
            seen += 1;
            if seen == 2 {
                return true;
            }
        }
        false
    }

    pub(crate) fn next(&mut self) -> Result<Option<Piece<'_>>, AssetError> {
        if !std::mem::take(&mut self.ahead)
            && self.decoder.next_frame_info().map_err(error)?.is_none()
        {
            return Ok(None);
        }
        let frame = self
            .decoder
            .current_frame_info()
            .ok_or_else(|| AssetError::Decode("a GIF frame without a header".to_owned()))?;
        let (x, y, w, h) = (frame.left, frame.top, frame.width, frame.height);
        // Its pixels are sized by the frame, not the screen: one past the
        // pixel cap is refused before they are allocated. One reaching past
        // the screen is clipped to it when composited.
        check_size(w.into(), h.into())?;
        let delay = Duration::from_millis(u64::from(frame.delay) * 10);
        let dispose = match frame.dispose {
            DisposalMethod::Background => Dispose::Clear,
            DisposalMethod::Previous => Dispose::Restore,
            _ => Dispose::Keep,
        };
        self.pixels.resize(self.decoder.buffer_size(), 0);
        self.decoder
            .read_into_buffer(&mut self.pixels)
            .map_err(error)?;
        Ok(Some(Piece::Region {
            x: x.into(),
            y: y.into(),
            w: w.into(),
            h: h.into(),
            pixels: &self.pixels,
            blend: Blend::Keyed,
            dispose,
            delay,
        }))
    }
}

/// A GIF's first frame, on its logical screen.
pub(crate) fn still(bytes: &[u8]) -> Result<RasterImage, AssetError> {
    let bytes = AssetBytes::Shared(bytes.into());
    let (frames, header) = GifFrames::open(&bytes)?;
    let (w, h) = check_size(header.width, header.height)?;
    let mut animation = Animation::from_frames(&bytes, Frames::Gif(Box::new(frames)), header)?;
    let frame = animation
        .next_frame()?
        .ok_or_else(|| AssetError::Decode("the GIF has no image".to_owned()))?;
    Ok(RasterImage::rgba8(w, h, frame.pixels.to_vec()))
}
