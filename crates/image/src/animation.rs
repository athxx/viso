//! An animated image's frames, decoded as they play: a GIF, an APNG, an
//! animated WebP or an AVIF image sequence.
//!
//! Each frame is composited onto the canvas the frames before it left — its
//! region drawn over or in place of what is there, then disposed of as the
//! format says once the next frame comes — so an animation of any length
//! holds one canvas, not every frame. [`Animation::next_frame`] hands each
//! frame as premultiplied RGBA8, the texture convention; [`Animation::rewind`]
//! starts the next play.
//!
//! A frame shows for its delay; a delay of 10 ms or less shows for 100 ms,
//! as browsers show it, since encoders write zero for "as fast as possible".

use std::num::NonZeroU32;
use std::time::Duration;

use viso_schema::resource::{AssetBytes, AssetFormat};

use crate::decode::{GifFrames, PngFrames, WebPFrames, apng_frames};
use crate::raster::{AssetError, check_size, premultiply_into};

/// How many times an animation plays.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Loops {
    Forever,
    /// This many plays in all, the last resting on its final frame.
    Times(NonZeroU32),
}

impl Loops {
    /// `plays` in all, none meaning forever.
    pub(crate) fn plays(plays: u32) -> Loops {
        NonZeroU32::new(plays).map_or(Loops::Forever, Loops::Times)
    }
}

/// One frame of an animation, as the canvas shows it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Frame<'a> {
    /// The canvas: `width × height` premultiplied RGBA8 texels, rows top
    /// first.
    pub pixels: &'a [u8],
    /// How long it shows.
    pub delay: Duration,
}

/// How a frame's region meets the canvas under it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Blend {
    /// Its texels replace the canvas's, transparent ones too.
    Replace,
    /// Over the canvas, by its alpha.
    Over,
    /// A GIF's: its transparent texels leave the canvas, the rest replace it.
    Keyed,
}

/// What becomes of a frame's region once the next frame comes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Dispose {
    /// It stays.
    Keep,
    /// It is cleared to transparent.
    Clear,
    /// The canvas under it comes back.
    Restore,
}

/// A frame a decoder hands the canvas.
pub(crate) enum Piece<'a> {
    /// A region of it: at `x, y` on the canvas, `w × h` straight RGBA8
    /// texels.
    Region {
        x: u32,
        y: u32,
        w: u32,
        h: u32,
        pixels: &'a [u8],
        blend: Blend,
        dispose: Dispose,
        delay: Duration,
    },
    /// The whole canvas, which the decoder composited: straight RGBA8.
    Whole { pixels: &'a [u8], delay: Duration },
}

/// What a frame decoder knows of its animation from its header.
pub(crate) struct Header {
    pub(crate) width: usize,
    pub(crate) height: usize,
    pub(crate) loops: Loops,
    /// How many frames it declares, where its format declares them.
    pub(crate) frames: Option<u32>,
}

/// The decoder of one format's frames, in order.
pub(crate) enum Frames {
    Gif(Box<GifFrames>),
    Png(Box<PngFrames>),
    WebP(Box<WebPFrames>),
    #[cfg(all(
        feature = "avif",
        not(any(target_arch = "wasm32", target_arch = "arm"))
    ))]
    Avif(Box<crate::decode::avif::AvifFrames>),
}

impl Frames {
    /// The frames of `bytes` with their header; `None` when the image is
    /// not animated.
    fn open(bytes: &AssetBytes) -> Result<Option<(Frames, Header)>, AssetError> {
        Ok(match AssetFormat::sniff(bytes) {
            Some(AssetFormat::Gif) => {
                let (frames, header) = GifFrames::open(bytes)?;
                (header.frames != Some(1)).then(|| (Frames::Gif(Box::new(frames)), header))
            }
            Some(AssetFormat::Png) => {
                apng_frames(bytes)?.map(|(frames, header)| (Frames::Png(Box::new(frames)), header))
            }
            Some(AssetFormat::WebP) => WebPFrames::open(bytes)?
                .map(|(frames, header)| (Frames::WebP(Box::new(frames)), header)),
            #[cfg(all(
                feature = "avif",
                not(any(target_arch = "wasm32", target_arch = "arm"))
            ))]
            Some(AssetFormat::Avif) => crate::decode::avif::AvifFrames::open(bytes)?
                .map(|(frames, header)| (Frames::Avif(Box::new(frames)), header)),
            _ => None,
        })
    }

    fn next(&mut self) -> Result<Option<Piece<'_>>, AssetError> {
        match self {
            Frames::Gif(frames) => frames.next(),
            Frames::Png(frames) => frames.next(),
            Frames::WebP(frames) => frames.next(),
            #[cfg(all(
                feature = "avif",
                not(any(target_arch = "wasm32", target_arch = "arm"))
            ))]
            Frames::Avif(frames) => frames.next(),
        }
    }
}

/// Whether `bytes` hold an animated image: more than one frame.
pub fn is_animated(bytes: &AssetBytes) -> bool {
    matches!(Frames::open(bytes), Ok(Some(_)))
}

/// An animated image, playing.
pub struct Animation {
    bytes: AssetBytes,
    frames: Frames,
    width: usize,
    height: usize,
    loops: Loops,
    count: Option<u32>,
    /// The canvas, straight alpha.
    canvas: Vec<u8>,
    /// The canvas under the last frame's region, kept to restore it.
    under: Vec<u8>,
    /// The last frame's region and what becomes of it.
    pending: Option<(Region, Dispose)>,
    /// The canvas premultiplied, as handed out.
    shown: Vec<u8>,
    /// How many frames this play has shown.
    played: u32,
}

/// A rectangle of the canvas.
#[derive(Debug, Clone, Copy)]
struct Region {
    x: usize,
    y: usize,
    w: usize,
    h: usize,
}

impl Animation {
    /// The animation `bytes` hold; `None` when they hold a still image (one
    /// [`crate::decode_raster_asset`] decodes).
    ///
    /// # Errors
    ///
    /// A decoder error, or a canvas past the raster limits.
    pub fn open(bytes: &AssetBytes) -> Result<Option<Animation>, AssetError> {
        let Some((frames, header)) = Frames::open(bytes)? else {
            return Ok(None);
        };
        Animation::from_frames(bytes, frames, header).map(Some)
    }

    /// The animation `frames` of `bytes` play, as `header` describes it.
    pub(crate) fn from_frames(
        bytes: &AssetBytes,
        frames: Frames,
        header: Header,
    ) -> Result<Animation, AssetError> {
        let (w, h) = check_size(header.width, header.height)?;
        let len = w as usize * h as usize * 4;
        Ok(Animation {
            bytes: bytes.clone(),
            frames,
            width: w as usize,
            height: h as usize,
            loops: header.loops,
            count: header.frames,
            canvas: vec![0; len],
            under: Vec::new(),
            pending: None,
            shown: vec![0; len],
            played: 0,
        })
    }

    /// Its canvas's size, in texels.
    pub fn size(&self) -> (u32, u32) {
        (self.width as u32, self.height as u32)
    }

    /// How many times it plays.
    pub fn loops(&self) -> Loops {
        self.loops
    }

    /// How many frames it declares, where its format declares them.
    pub fn frame_count(&self) -> Option<u32> {
        self.count
    }

    /// The next frame of this play; `None` once the play has shown every
    /// frame.
    ///
    /// # Errors
    ///
    /// A decoder error; the frames before it played.
    pub fn next_frame(&mut self) -> Result<Option<Frame<'_>>, AssetError> {
        // The last frame's region goes as it said.
        match self.pending.take() {
            Some((r, Dispose::Clear)) => self.each_row(r, |row| row.fill(0)),
            Some((r, Dispose::Restore)) => {
                let w = self.width;
                for (i, row) in self.under.chunks_exact(r.w * 4).enumerate() {
                    let at = ((r.y + i) * w + r.x) * 4;
                    self.canvas[at..at + row.len()].copy_from_slice(row);
                }
            }
            _ => {}
        }
        let first = self.played == 0;
        let (canvas, under, (width, height)) =
            (&mut self.canvas, &mut self.under, (self.width, self.height));
        let Some(piece) = self.frames.next()? else {
            return Ok(None);
        };
        let delay = match piece {
            Piece::Whole { pixels, delay } => {
                if pixels.len() != canvas.len() {
                    return Err(AssetError::Decode(
                        "a frame is not the canvas's size".to_owned(),
                    ));
                }
                canvas.copy_from_slice(pixels);
                delay
            }
            Piece::Region {
                x,
                y,
                w,
                h,
                pixels,
                blend,
                dispose,
                delay,
            } => {
                let (x, y, fw, fh) = (x as usize, y as usize, w as usize, h as usize);
                if pixels.len() != fw * fh * 4 {
                    return Err(AssetError::Decode(
                        "a frame's texels are not its size".to_owned(),
                    ));
                }
                // The region as it lies on the canvas; past its edge is cut.
                let region = Region {
                    x: x.min(width),
                    y: y.min(height),
                    w: fw.min(width.saturating_sub(x)),
                    h: fh.min(height.saturating_sub(y)),
                };
                // A first frame has nothing under it to restore.
                let dispose = match dispose {
                    Dispose::Restore if first => Dispose::Clear,
                    d => d,
                };
                if dispose == Dispose::Restore {
                    under.clear();
                    for row in 0..region.h {
                        let at = ((region.y + row) * width + region.x) * 4;
                        under.extend_from_slice(&canvas[at..at + region.w * 4]);
                    }
                }
                for row in 0..region.h {
                    let src = &pixels[row * fw * 4..][..region.w * 4];
                    let at = ((region.y + row) * width + region.x) * 4;
                    let dst = &mut canvas[at..at + region.w * 4];
                    draw(dst, src, blend);
                }
                self.pending = Some((region, dispose));
                delay
            }
        };
        self.played += 1;
        premultiply_into(&self.canvas, &mut self.shown);
        Ok(Some(Frame {
            pixels: &self.shown,
            delay: shown_for(delay),
        }))
    }

    /// Starts the next play at its first frame, on a clear canvas.
    ///
    /// # Errors
    ///
    /// The bytes no longer open, which they did at [`open`](Self::open).
    pub fn rewind(&mut self) -> Result<(), AssetError> {
        let (frames, _) = Frames::open(&self.bytes)?
            .ok_or_else(|| AssetError::Decode("the animation is gone".to_owned()))?;
        self.frames = frames;
        self.canvas.fill(0);
        self.pending = None;
        self.played = 0;
        Ok(())
    }

    fn each_row(&mut self, r: Region, mut f: impl FnMut(&mut [u8])) {
        for row in 0..r.h {
            let at = ((r.y + row) * self.width + r.x) * 4;
            f(&mut self.canvas[at..at + r.w * 4]);
        }
    }
}

/// How long a frame of `delay` shows: 100 ms for 10 ms or less.
fn shown_for(delay: Duration) -> Duration {
    if delay <= Duration::from_millis(10) {
        Duration::from_millis(100)
    } else {
        delay
    }
}

/// Draws the straight RGBA8 row `src` onto the canvas row `dst` by `blend`.
fn draw(dst: &mut [u8], src: &[u8], blend: Blend) {
    match blend {
        Blend::Replace => dst.copy_from_slice(src),
        Blend::Keyed => {
            for (d, s) in dst
                .as_chunks_mut::<4>()
                .0
                .iter_mut()
                .zip(src.as_chunks::<4>().0.iter())
            {
                if s[3] != 0 {
                    d.copy_from_slice(s);
                }
            }
        }
        Blend::Over => {
            for (d, s) in dst
                .as_chunks_mut::<4>()
                .0
                .iter_mut()
                .zip(src.as_chunks::<4>().0.iter())
            {
                match s[3] {
                    0 => {}
                    255 => d.copy_from_slice(s),
                    sa => {
                        // Straight-alpha OVER on the stored values, as
                        // browsers composite APNG frames.
                        let sa = u32::from(sa);
                        let da = u32::from(d[3]) * (255 - sa) / 255;
                        let a = sa + da;
                        for c in 0..3 {
                            let v = (u32::from(s[c]) * sa + u32::from(d[c]) * da + a / 2) / a;
                            d[c] = v as u8;
                        }
                        d[3] = a as u8;
                    }
                }
            }
        }
    }
}

// An animation decodes off the thread that opened it.
const _: () = {
    const fn send<T: Send>() {}
    send::<Animation>();
};

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn over_blends_by_alpha_and_keyed_leaves_the_transparent() {
        let mut dst = [10, 20, 30, 255, 0, 0, 0, 0];
        draw(&mut dst, &[200, 100, 0, 128, 50, 60, 70, 0], Blend::Over);
        assert_eq!(dst[3], 255);
        assert!((dst[0] as i32 - 105).abs() <= 1, "{dst:?}");
        assert_eq!(&dst[4..], [0, 0, 0, 0], "a transparent texel leaves it");
        let mut dst = [1, 2, 3, 255, 4, 5, 6, 255];
        draw(&mut dst, &[9, 9, 9, 0, 7, 7, 7, 255], Blend::Keyed);
        assert_eq!(dst, [1, 2, 3, 255, 7, 7, 7, 255]);
        let mut dst = [1, 2, 3, 255];
        draw(&mut dst, &[9, 9, 9, 0], Blend::Replace);
        assert_eq!(dst, [9, 9, 9, 0]);
    }

    #[test]
    fn a_fast_frame_shows_for_a_tenth_of_a_second() {
        assert_eq!(shown_for(Duration::ZERO), Duration::from_millis(100));
        assert_eq!(
            shown_for(Duration::from_millis(10)),
            Duration::from_millis(100)
        );
        assert_eq!(
            shown_for(Duration::from_millis(20)),
            Duration::from_millis(20)
        );
    }
}
