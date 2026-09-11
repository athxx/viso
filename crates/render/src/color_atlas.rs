//! The color-glyph atlas: an RGBA8 texture the renderer packs premultiplied
//! color-emoji bitmaps into, plus the CPU backing it uploads from.
//!
//! Structurally a sibling of [`GlyphAtlas`](crate::glyph_atlas::GlyphAtlas): the
//! same max-rects packer and generational-wipe lifecycle, but four bytes per
//! texel ([`TextureFormat::Rgba8Unorm`]) instead of one. Color glyphs
//! ([`ColorGlyph`]) carry their own premultiplied RGBA, so they lower through
//! the Image primitive with a white tint rather than the coverage shader; this
//! atlas is where those bitmaps live.

use viso_gpu::{TextureFormat, TextureId};
use viso_text::system_fonts::ColorGlyph;

use crate::primitive::Rect;
use crate::rect_packer::{RectPacker, TexelRect};

/// Bytes per texel of the RGBA8 backing.
const BPT: u32 = 4;

/// An RGBA8 color-glyph atlas: a packer, a CPU pixel backing, and the
/// accumulated dirty rect the renderer uploads.
///
/// The GPU [`TextureId`] is created once by the caller (the renderer owns
/// device resource creation) and handed in; this type never touches the device.
#[derive(Debug)]
pub struct ColorAtlas {
    /// Atlas edge length in texels (square, `size × size`).
    size: u32,
    /// The free-rect packer.
    packer: RectPacker,
    /// Row-major RGBA8 pixels, `size² × 4` bytes; the CPU source of truth.
    pixels: Vec<u8>,
    /// The GPU texture (`Rgba8Unorm`, `size × size`) these pixels back.
    texture: TextureId,
    /// Accumulated dirty rect since the last [`take_dirty`](Self::take_dirty),
    /// or `None` when nothing changed.
    dirty: Option<TexelRect>,
    /// Generational epoch; bumped on every overflow wipe so callers invalidate
    /// UV caches keyed on it.
    epoch: u32,
}

/// The result of [`ColorAtlas::alloc`].
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum ColorAlloc {
    /// The glyph was packed at this normalized UV sub-rect.
    Placed(Rect),
    /// The glyph was empty (zero-area bitmap); nothing packed.
    Empty,
    /// The glyph did not fit; the atlas wiped generationally (epoch bumped) and
    /// the caller must re-pack live glyphs and retry this one.
    Overflow,
}

impl ColorAtlas {
    /// A fresh atlas of `size × size` texels, backing the given GPU texture.
    ///
    /// The texture must be created as [`TextureFormat::Rgba8Unorm`] at the same
    /// dimensions. The backing starts fully zero (transparent).
    pub fn new(size: u32, texture: TextureId) -> Self {
        Self {
            size,
            packer: RectPacker::new(size),
            pixels: vec![0u8; (size as usize) * (size as usize) * BPT as usize],
            texture,
            dirty: None,
            epoch: 0,
        }
    }

    /// The RGBA8 pixel format a color-atlas texture must be created with.
    pub const FORMAT: TextureFormat = TextureFormat::Rgba8Unorm;

    /// Atlas edge length in texels.
    pub fn size(&self) -> u32 {
        self.size
    }

    /// The GPU texture handle these pixels back.
    pub fn texture(&self) -> TextureId {
        self.texture
    }

    /// The current generation. Bumps on every overflow wipe; callers key their
    /// per-glyph UV caches on it and re-pack after a change.
    pub fn epoch(&self) -> u32 {
        self.epoch
    }

    /// The full CPU pixel backing (row-major RGBA8, `size² × 4` bytes).
    pub fn pixels(&self) -> &[u8] {
        &self.pixels
    }

    /// Pack `glyph`'s premultiplied RGBA into the atlas, returning where it
    /// landed.
    ///
    /// A zero-area bitmap is [`ColorAlloc::Empty`] with no allocation. On success
    /// the RGBA is blitted into the CPU backing (dirty rect grown) and the
    /// normalized UV sub-rect is returned. On overflow the atlas wipes
    /// generationally and returns [`ColorAlloc::Overflow`]; the caller re-packs
    /// and retries.
    pub fn alloc(&mut self, glyph: &ColorGlyph) -> ColorAlloc {
        let gw = glyph.width;
        let gh = glyph.height;
        if gw == 0 || gh == 0 || glyph.rgba.is_empty() {
            return ColorAlloc::Empty;
        }
        // 1-texel gutter on all sides so bilinear sampling never bleeds a
        // neighbor into this glyph's fringe.
        let pad_w = gw + 2;
        let pad_h = gh + 2;
        if pad_w > self.size || pad_h > self.size {
            // Larger than the atlas even alone: a wipe would not help.
            return ColorAlloc::Overflow;
        }

        let (px, py) = match self.packer.allocate(pad_w, pad_h) {
            Some(origin) => origin,
            None => {
                self.wipe();
                return ColorAlloc::Overflow;
            }
        };
        // The glyph sits one texel in from the padded cell's origin.
        let gx = px + 1;
        let gy = py + 1;
        self.blit(gx, gy, glyph);
        self.grow_dirty(TexelRect {
            x: gx,
            y: gy,
            w: gw,
            h: gh,
        });

        let inv = 1.0 / self.size as f32;
        ColorAlloc::Placed(Rect {
            x: gx as f32 * inv,
            y: gy as f32 * inv,
            w: gw as f32 * inv,
            h: gh as f32 * inv,
        })
    }

    /// Take the accumulated dirty sub-rect (in texels) and its RGBA bytes,
    /// resetting the dirty state. Returns `None` when nothing changed.
    ///
    /// The bytes are the tightly-packed rows of the dirty sub-rect (`w × 4`
    /// bytes per row), suitable for
    /// [`viso_gpu::GpuBackend::write_texture`] at `(x, y, w, h)`.
    pub fn take_dirty(&mut self) -> Option<(u32, u32, u32, u32, Vec<u8>)> {
        let d = self.dirty.take()?;
        let row_bytes = (d.w * BPT) as usize;
        let mut bytes = Vec::with_capacity(row_bytes * d.h as usize);
        for row in 0..d.h {
            let start = ((d.y + row) * self.size + d.x) as usize * BPT as usize;
            bytes.extend_from_slice(&self.pixels[start..start + row_bytes]);
        }
        Some((d.x, d.y, d.w, d.h, bytes))
    }

    /// Blit a color glyph's RGBA into the CPU backing at texel `(x, y)`.
    fn blit(&mut self, x: u32, y: u32, glyph: &ColorGlyph) {
        let src_row = (glyph.width * BPT) as usize;
        for row in 0..glyph.height {
            let src = (row * glyph.width * BPT) as usize;
            let dst = ((y + row) * self.size + x) as usize * BPT as usize;
            self.pixels[dst..dst + src_row].copy_from_slice(&glyph.rgba[src..src + src_row]);
        }
    }

    /// Union `rect` into the accumulated dirty rect.
    fn grow_dirty(&mut self, rect: TexelRect) {
        self.dirty = Some(match self.dirty {
            None => rect,
            Some(d) => {
                let x = d.x.min(rect.x);
                let y = d.y.min(rect.y);
                let max_x = d.max_x().max(rect.max_x());
                let max_y = d.max_y().max(rect.max_y());
                TexelRect {
                    x,
                    y,
                    w: max_x - x,
                    h: max_y - y,
                }
            }
        });
    }

    /// Generational wipe: reset the packer, clear the backing, mark the whole
    /// texture dirty, and bump the epoch.
    fn wipe(&mut self) {
        self.packer.reset();
        self.pixels.iter_mut().for_each(|p| *p = 0);
        self.dirty = Some(TexelRect {
            x: 0,
            y: 0,
            w: self.size,
            h: self.size,
        });
        self.epoch = self.epoch.wrapping_add(1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn glyph(w: u32, h: u32, fill: [u8; 4]) -> ColorGlyph {
        let mut rgba = Vec::with_capacity((w * h * 4) as usize);
        for _ in 0..(w * h) {
            rgba.extend_from_slice(&fill);
        }
        ColorGlyph {
            width: w,
            height: h,
            rgba,
            pixels_per_em: 48,
            origin_px: [0.0, 0.0],
        }
    }

    #[test]
    fn empty_glyph_allocates_nothing() {
        let mut atlas = ColorAtlas::new(64, TextureId(0));
        assert_eq!(atlas.alloc(&glyph(0, 0, [0; 4])), ColorAlloc::Empty);
        assert!(atlas.take_dirty().is_none());
    }

    #[test]
    fn placed_uv_is_normalized_and_gutter_offset() {
        let mut atlas = ColorAtlas::new(64, TextureId(0));
        let ColorAlloc::Placed(uv) = atlas.alloc(&glyph(8, 8, [10, 20, 30, 40])) else {
            panic!("should place");
        };
        assert_eq!(uv.x, 1.0 / 64.0);
        assert_eq!(uv.y, 1.0 / 64.0);
        assert_eq!(uv.w, 8.0 / 64.0);
        assert_eq!(uv.h, 8.0 / 64.0);
    }

    #[test]
    fn blit_writes_rgba_and_dirty_rect() {
        let mut atlas = ColorAtlas::new(64, TextureId(0));
        atlas.alloc(&glyph(4, 4, [1, 2, 3, 4]));
        let (x, y, w, h, bytes) = atlas.take_dirty().expect("dirty after alloc");
        assert_eq!((x, y, w, h), (1, 1, 4, 4));
        assert_eq!(bytes.len(), 4 * 4 * 4);
        assert_eq!(&bytes[0..4], &[1, 2, 3, 4]);
        assert!(atlas.take_dirty().is_none());
    }

    #[test]
    fn overflow_wipes_and_bumps_epoch() {
        // A tiny atlas that fits one padded 8×8 (10×10) but not two.
        let mut atlas = ColorAtlas::new(12, TextureId(0));
        assert!(matches!(
            atlas.alloc(&glyph(8, 8, [9, 9, 9, 9])),
            ColorAlloc::Placed(_)
        ));
        assert_eq!(atlas.epoch(), 0);
        assert_eq!(
            atlas.alloc(&glyph(8, 8, [9, 9, 9, 9])),
            ColorAlloc::Overflow
        );
        assert_eq!(atlas.epoch(), 1);
        let (x, y, w, h, _) = atlas.take_dirty().expect("wipe dirties all");
        assert_eq!((x, y, w, h), (0, 0, 12, 12));
        assert!(matches!(
            atlas.alloc(&glyph(8, 8, [9, 9, 9, 9])),
            ColorAlloc::Placed(_)
        ));
    }

    #[test]
    fn glyph_larger_than_atlas_is_overflow_without_wipe_loop() {
        let mut atlas = ColorAtlas::new(8, TextureId(0));
        assert_eq!(
            atlas.alloc(&glyph(8, 8, [1, 1, 1, 1])),
            ColorAlloc::Overflow
        );
        assert_eq!(atlas.epoch(), 0);
    }
}
