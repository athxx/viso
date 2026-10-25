//! Mouse cursors an app names over its nodes: the system shapes, and images
//! it registers as cursors of its own.
//!
//! An image cursor is registered once and named after that by a compact,
//! copyable id, like any shape:
//!
//! ```no_run
//! use viso::cursor::{CursorIcon, CustomCursor};
//!
//! # fn png() -> Vec<u8> { unimplemented!() }
//! let image = viso::cursor::decode(&png(), [4, 4]).expect("a cursor image");
//! let crosshair = CustomCursor::new(image.with_scale(2.0));
//! let icon = CursorIcon::Custom(crosshair);
//! # let _ = icon;
//! ```
//!
//! A node shows it with `cx.cursor(node, icon)`. Each platform builds its
//! OS cursor the first time a window shows it and reuses it after; one that
//! shows no image cursors (`PlatformCaps::custom_cursors`) shows the
//! default arrow instead.

pub use viso_platform::{CursorError, CursorIcon, CursorImage, CustomCursor, MAX_CURSOR_SIDE};

/// Why encoded bytes make no cursor image.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DecodeError {
    /// The bytes did not decode as an image.
    Image(viso_image::AssetError),
    /// The image decoded but makes no cursor: too large, or the hotspot
    /// lies outside it.
    Cursor(CursorError),
}

impl std::fmt::Display for DecodeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            DecodeError::Image(e) => e.fmt(f),
            DecodeError::Cursor(e) => e.fmt(f),
        }
    }
}

impl std::error::Error for DecodeError {}

/// The cursor image in `bytes` — PNG, WebP, GIF, BMP, ICO or any format an
/// image asset may be in — pointing with pixel `hotspot` (`[x, y]` from the
/// top left), drawn one pixel to a point until
/// [`with_scale`](CursorImage::with_scale) says otherwise.
///
/// # Errors
///
/// Bytes that do not decode, or an image past [`MAX_CURSOR_SIDE`] or with
/// the hotspot outside it.
pub fn decode(bytes: &[u8], hotspot: [u32; 2]) -> Result<CursorImage, DecodeError> {
    let raster = viso_image::decode_raster(bytes).map_err(DecodeError::Image)?;
    let (width, height) = (raster.width, raster.height);
    if width > MAX_CURSOR_SIDE || height > MAX_CURSOR_SIDE {
        return Err(DecodeError::Cursor(CursorError::Size { width, height }));
    }
    let texels = raster
        .rgba8_texels()
        .ok_or(DecodeError::Image(viso_image::AssetError::Format))?;
    // Texels are premultiplied; a cursor takes straight colour.
    let rgba: Vec<u8> = texels
        .into_iter()
        .flat_map(|[r, g, b, a]| {
            let un = |c: u8| {
                if a == 0 {
                    0
                } else {
                    ((u16::from(c) * 255 + u16::from(a) / 2) / u16::from(a)).min(255) as u8
                }
            };
            [un(r), un(g), un(b), a]
        })
        .collect();
    CursorImage::new(width, height, rgba, hotspot).map_err(DecodeError::Cursor)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A 2×1 QOI image: an opaque red pixel and a half-clear white one.
    fn qoi() -> Vec<u8> {
        let mut out = b"qoif".to_vec();
        out.extend_from_slice(&2u32.to_be_bytes());
        out.extend_from_slice(&1u32.to_be_bytes());
        out.extend_from_slice(&[4, 0]);
        out.extend_from_slice(&[0xff, 255, 0, 0, 255]);
        out.extend_from_slice(&[0xff, 255, 255, 255, 128]);
        out.extend_from_slice(&[0, 0, 0, 0, 0, 0, 0, 1]);
        out
    }

    #[test]
    fn an_encoded_image_decodes_to_straight_pixels() {
        let image = decode(&qoi(), [1, 0]).expect("decodes");
        assert_eq!((image.width(), image.height()), (2, 1));
        assert_eq!(image.hotspot(), [1, 0]);
        assert_eq!(&image.rgba()[..4], &[255, 0, 0, 255]);
        let [r, g, b, a] = image.rgba()[4..8] else {
            unreachable!()
        };
        assert_eq!(a, 128);
        assert!(
            r >= 254 && g >= 254 && b >= 254,
            "straight white: {r} {g} {b}"
        );
    }

    #[test]
    fn a_hotspot_outside_the_image_is_refused() {
        assert_eq!(
            decode(&qoi(), [2, 0]),
            Err(DecodeError::Cursor(CursorError::Hotspot { x: 2, y: 0 }))
        );
        assert!(matches!(
            decode(b"not an image", [0, 0]),
            Err(DecodeError::Image(_))
        ));
    }
}
