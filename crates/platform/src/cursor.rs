//! Mouse cursors: the system shapes a window shows over its content, and
//! images an app registers as cursors of its own.
//!
//! An image cursor is registered once ([`CustomCursor::new`]) and named after
//! that by a compact id, as cheap to copy and compare as a system shape: the
//! registry keeps the pixels for the life of the process and each backend
//! builds its OS cursor from them the first time a window shows it, then
//! reuses it. Registering the same image again returns the same id, so a
//! view rebuilt with the same cursor does not grow the registry.

use std::borrow::Cow;
use std::num::NonZeroU32;
use std::sync::{Arc, Mutex};

/// The mouse cursor shape a window shows over its content.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum CursorIcon {
    #[default]
    Default,
    /// A link / clickable affordance (pointing hand).
    Pointer,
    Text,
    VerticalText,
    Crosshair,
    Move,
    Grab,
    Grabbing,
    NotAllowed,
    Wait,
    /// Busy but still interactive.
    Progress,
    Help,
    ContextMenu,
    Copy,
    Alias,
    ZoomIn,
    ZoomOut,
    /// Horizontal resize (`↔`).
    ResizeEw,
    /// Vertical resize (`↕`).
    ResizeNs,
    /// Diagonal resize (`↗↙`).
    ResizeNesw,
    /// Diagonal resize (`↖↘`).
    ResizeNwse,
    /// Column splitter.
    ResizeCol,
    /// Row splitter.
    ResizeRow,
    /// No cursor at all.
    Hidden,
    /// An image the app registered. Where the platform shows no image
    /// cursors ([`PlatformCaps::custom_cursors`](crate::PlatformCaps) is
    /// false) the window shows the default arrow instead.
    Custom(CustomCursor),
}

/// The longest side a cursor image may have, in pixels. Browsers draw at
/// most 128 CSS pixels and may refuse a cursor over 32 near their own
/// controls; desktop systems take the full size.
pub const MAX_CURSOR_SIDE: u32 = 256;

/// Why pixels make no cursor image.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CursorError {
    /// A side is zero or longer than [`MAX_CURSOR_SIDE`].
    Size { width: u32, height: u32 },
    /// The pixels are not `width × height × 4` bytes.
    Pixels { expected: usize, got: usize },
    /// The hotspot lies outside the image.
    Hotspot { x: u32, y: u32 },
}

impl std::fmt::Display for CursorError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match *self {
            CursorError::Size { width, height } => write!(
                f,
                "a {width}×{height} cursor: each side must be 1 to {MAX_CURSOR_SIDE} pixels"
            ),
            CursorError::Pixels { expected, got } => {
                write!(f, "a cursor of {expected} RGBA bytes was given {got}")
            }
            CursorError::Hotspot { x, y } => {
                write!(f, "the cursor's hotspot ({x}, {y}) lies outside its image")
            }
        }
    }
}

impl std::error::Error for CursorError {}

/// A cursor's pixels: straight (not premultiplied) sRGB RGBA, row by row from
/// the top, the hotspot the pixel that points, and how many pixels make one
/// logical point.
#[derive(Debug, Clone, PartialEq)]
pub struct CursorImage {
    width: u16,
    height: u16,
    hotspot: [u16; 2],
    scale: f32,
    rgba: Box<[u8]>,
}

impl CursorImage {
    /// A `width × height` image of straight RGBA bytes, pointing with pixel
    /// `hotspot` (`[x, y]` from the top left), drawn one pixel to a point.
    ///
    /// # Errors
    ///
    /// A side of zero or past [`MAX_CURSOR_SIDE`], pixels of another length,
    /// or a hotspot outside the image.
    pub fn new(
        width: u32,
        height: u32,
        rgba: impl Into<Box<[u8]>>,
        hotspot: [u32; 2],
    ) -> Result<CursorImage, CursorError> {
        let rgba = rgba.into();
        if width == 0 || height == 0 || width > MAX_CURSOR_SIDE || height > MAX_CURSOR_SIDE {
            return Err(CursorError::Size { width, height });
        }
        let expected = width as usize * height as usize * 4;
        if rgba.len() != expected {
            return Err(CursorError::Pixels {
                expected,
                got: rgba.len(),
            });
        }
        let [x, y] = hotspot;
        if x >= width || y >= height {
            return Err(CursorError::Hotspot { x, y });
        }
        Ok(CursorImage {
            width: width as u16,
            height: height as u16,
            hotspot: [x as u16, y as u16],
            scale: 1.0,
            rgba,
        })
    }

    /// The image drawn `scale` pixels to a logical point: a 64-pixel image at
    /// scale 2 shows 32 points wide, sharp on a display of that density. A
    /// scale that is not a positive finite number leaves it at one.
    pub fn with_scale(mut self, scale: f32) -> CursorImage {
        if scale.is_finite() && scale > 0.0 {
            self.scale = scale;
        }
        self
    }

    /// Its width in pixels.
    pub fn width(&self) -> u32 {
        u32::from(self.width)
    }

    /// Its height in pixels.
    pub fn height(&self) -> u32 {
        u32::from(self.height)
    }

    /// The pixel that points, `[x, y]` from the top left.
    pub fn hotspot(&self) -> [u32; 2] {
        self.hotspot.map(u32::from)
    }

    /// Pixels to a logical point.
    pub fn scale(&self) -> f32 {
        self.scale
    }

    /// Its straight RGBA bytes, row by row from the top.
    pub fn rgba(&self) -> &[u8] {
        &self.rgba
    }

    /// The image as a display of `scale` pixels to a point shows it: its
    /// pixels resampled to that density (borrowed when it already has it),
    /// with its size and hotspot there.
    pub fn at_scale(&self, scale: f64) -> ScaledCursor<'_> {
        let k = if scale.is_finite() && scale > 0.0 {
            scale / f64::from(self.scale)
        } else {
            1.0 / f64::from(self.scale)
        };
        let side = |n: u16| ((f64::from(n) * k).round() as u32).clamp(1, MAX_CURSOR_SIDE);
        let (w, h) = (side(self.width), side(self.height));
        let hot = |at: u16, n: u32| ((f64::from(at) * k).round() as u32).min(n - 1);
        let hotspot = [hot(self.hotspot[0], w), hot(self.hotspot[1], h)];
        let rgba = if (w, h) == (self.width(), self.height()) {
            Cow::Borrowed(&self.rgba[..])
        } else {
            Cow::Owned(resample(&self.rgba, self.width(), self.height(), w, h))
        };
        ScaledCursor {
            width: w,
            height: h,
            hotspot,
            rgba,
        }
    }
}

/// A cursor image at a display's density, as [`CursorImage::at_scale`] makes it.
#[derive(Debug)]
pub struct ScaledCursor<'a> {
    pub width: u32,
    pub height: u32,
    pub hotspot: [u32; 2],
    /// Straight RGBA bytes, row by row from the top.
    pub rgba: Cow<'a, [u8]>,
}

/// Resizes straight RGBA pixels from `w × h` to `to_w × to_h`: each output
/// pixel averages the source area it covers (bilinear when enlarging), in
/// premultiplied form so clear pixels lend no colour.
fn resample(src: &[u8], w: u32, h: u32, to_w: u32, to_h: u32) -> Vec<u8> {
    // The source spans each output pixel covers along one axis, with weights.
    fn spans(from: u32, to: u32) -> Vec<(usize, Vec<f32>)> {
        let k = from as f32 / to as f32;
        (0..to)
            .map(|i| {
                if k <= 1.0 {
                    // Enlarging: the two nearest source pixels, by distance.
                    let c = ((i as f32 + 0.5) * k - 0.5).clamp(0.0, (from - 1) as f32);
                    let lo = c.floor();
                    let t = c - lo;
                    if t == 0.0 || lo as u32 + 1 >= from {
                        (lo as usize, vec![1.0])
                    } else {
                        (lo as usize, vec![1.0 - t, t])
                    }
                } else {
                    // Shrinking: every source pixel the output covers, by overlap.
                    let (a, b) = (i as f32 * k, (i as f32 + 1.0) * k);
                    let first = a.floor() as usize;
                    let last = (b.ceil() as usize).min(from as usize);
                    let weights = (first..last)
                        .map(|s| (b.min(s as f32 + 1.0) - a.max(s as f32)) / k)
                        .collect();
                    (first, weights)
                }
            })
            .collect()
    }
    let xs = spans(w, to_w);
    let ys = spans(h, to_h);
    let px = |x: usize, y: usize| {
        let i = (y * w as usize + x) * 4;
        let a = f32::from(src[i + 3]) / 255.0;
        [
            f32::from(src[i]) * a,
            f32::from(src[i + 1]) * a,
            f32::from(src[i + 2]) * a,
            a,
        ]
    };
    let mut out = Vec::with_capacity(to_w as usize * to_h as usize * 4);
    for (y0, wy) in &ys {
        for (x0, wx) in &xs {
            let mut acc = [0.0f32; 4];
            for (dy, ky) in wy.iter().enumerate() {
                for (dx, kx) in wx.iter().enumerate() {
                    let p = px(x0 + dx, y0 + dy);
                    let k = kx * ky;
                    for c in 0..4 {
                        acc[c] += p[c] * k;
                    }
                }
            }
            let a = acc[3].clamp(0.0, 1.0);
            let un = |v: f32| {
                if a > 0.0 {
                    (v / a).round().clamp(0.0, 255.0) as u8
                } else {
                    0
                }
            };
            out.extend_from_slice(&[
                un(acc[0]),
                un(acc[1]),
                un(acc[2]),
                (a * 255.0).round() as u8,
            ]);
        }
    }
    out
}

/// An image cursor the app registered, named by a compact id: copied and
/// compared like a system shape, its OS cursor built once per backend.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct CustomCursor(NonZeroU32);

/// Every image registered, at its id less one. Touched only to register and
/// when a backend first builds a cursor.
static REGISTRY: Mutex<Vec<Arc<CursorImage>>> = Mutex::new(Vec::new());

impl CustomCursor {
    /// Registers `image` as a cursor for the rest of the process; an image
    /// registered before, pixel for pixel, gives back its id.
    pub fn new(image: CursorImage) -> CustomCursor {
        let mut images = REGISTRY.lock().unwrap_or_else(|e| e.into_inner());
        let at = match images.iter().position(|known| **known == image) {
            Some(at) => at,
            None => {
                images.push(Arc::new(image));
                images.len() - 1
            }
        };
        CustomCursor(NonZeroU32::new(at as u32 + 1).expect("one past an index"))
    }

    /// The image it shows.
    pub fn image(self) -> Arc<CursorImage> {
        let images = REGISTRY.lock().unwrap_or_else(|e| e.into_inner());
        images[self.index()].clone()
    }

    /// Its slot in a dense table indexed by cursor: 0 for the first registered.
    pub fn index(self) -> usize {
        self.0.get() as usize - 1
    }
}

/// The OS cursors a backend built from registered images, at each image's
/// index: built the first time a window shows one, reused after; one the OS
/// refused is remembered so it is not built again.
#[cfg_attr(
    not(any(
        target_os = "macos",
        all(target_arch = "wasm32", target_os = "unknown")
    )),
    allow(dead_code)
)]
pub(crate) struct CursorCache<T> {
    built: Vec<Option<Option<T>>>,
}

#[cfg_attr(
    not(any(
        target_os = "macos",
        all(target_arch = "wasm32", target_os = "unknown")
    )),
    allow(dead_code)
)]
impl<T> CursorCache<T> {
    pub(crate) const fn new() -> CursorCache<T> {
        CursorCache { built: Vec::new() }
    }

    /// The OS cursor for `cursor`, building it from its image with `build`
    /// the first time; `None` where the OS refused it.
    pub(crate) fn get(
        &mut self,
        cursor: CustomCursor,
        build: impl FnOnce(&CursorImage) -> Option<T>,
    ) -> Option<&T> {
        let at = cursor.index();
        if self.built.len() <= at {
            self.built.resize_with(at + 1, || None);
        }
        self.built[at]
            .get_or_insert_with(|| build(&cursor.image()))
            .as_ref()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn solid(w: u32, h: u32, px: [u8; 4]) -> Vec<u8> {
        px.repeat((w * h) as usize)
    }

    #[test]
    fn an_image_is_checked() {
        assert_eq!(
            CursorImage::new(0, 4, vec![], [0, 0]),
            Err(CursorError::Size {
                width: 0,
                height: 4
            })
        );
        assert_eq!(
            CursorImage::new(300, 4, vec![], [0, 0]).unwrap_err(),
            CursorError::Size {
                width: 300,
                height: 4
            }
        );
        assert_eq!(
            CursorImage::new(2, 2, vec![0; 15], [0, 0]),
            Err(CursorError::Pixels {
                expected: 16,
                got: 15
            })
        );
        assert_eq!(
            CursorImage::new(2, 2, vec![0; 16], [2, 0]),
            Err(CursorError::Hotspot { x: 2, y: 0 })
        );
        let ok = CursorImage::new(2, 3, vec![0; 24], [1, 2]).unwrap();
        assert_eq!((ok.width(), ok.height(), ok.hotspot()), (2, 3, [1, 2]));
        assert_eq!(ok.with_scale(f32::NAN).scale(), 1.0);
    }

    #[test]
    fn the_same_image_registers_once() {
        let image = |px| CursorImage::new(3, 3, solid(3, 3, px), [1, 1]).unwrap();
        let a = CustomCursor::new(image([9, 8, 7, 255]));
        let b = CustomCursor::new(image([9, 8, 7, 255]));
        let c = CustomCursor::new(image([9, 8, 7, 128]));
        assert_eq!(a, b);
        assert_ne!(a, c);
        assert_eq!(a.image().rgba()[..4], [9, 8, 7, 255]);
        assert_eq!(c.image().rgba()[3], 128);
    }

    #[test]
    fn an_image_shows_at_the_display_density() {
        let image = CursorImage::new(4, 4, solid(4, 4, [255, 0, 0, 255]), [3, 1]).unwrap();
        let same = image.at_scale(1.0);
        assert!(matches!(same.rgba, Cow::Borrowed(_)));
        let double = image.at_scale(2.0);
        assert_eq!(
            (double.width, double.height, double.hotspot),
            (8, 8, [6, 2])
        );
        assert_eq!(&double.rgba[..4], &[255, 0, 0, 255]);
        let dense = image.clone().with_scale(2.0);
        let sharp = dense.at_scale(1.0);
        assert_eq!(
            (sharp.width, sharp.height, sharp.hotspot),
            (2, 2, [2 - 1, 1])
        );
        assert_eq!(&sharp.rgba[..4], &[255, 0, 0, 255]);
    }

    #[test]
    fn shrinking_averages_without_bleeding_clear_colour() {
        // A 2×1 image: opaque white beside clear black, shrunk to one pixel.
        let src = [255, 255, 255, 255, 0, 0, 0, 0];
        assert_eq!(resample(&src, 2, 1, 1, 1), vec![255, 255, 255, 128]);
    }

    #[test]
    fn a_cache_builds_each_cursor_once() {
        let cursor = CustomCursor::new(CursorImage::new(1, 1, vec![1, 2, 3, 4], [0, 0]).unwrap());
        let mut cache = CursorCache::new();
        let mut builds = 0;
        for _ in 0..3 {
            let got = cache.get(cursor, |image| {
                builds += 1;
                Some(image.rgba()[0])
            });
            assert_eq!(got, Some(&1));
        }
        assert_eq!(builds, 1);
        let refused = CustomCursor::new(CursorImage::new(1, 1, vec![5, 6, 7, 8], [0, 0]).unwrap());
        assert_eq!(cache.get(refused, |_| None::<u8>), None);
        assert_eq!(cache.get(refused, |_| Some(0)), None, "not built again");
    }
}
