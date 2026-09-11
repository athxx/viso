//! The glyph atlas: a single-channel A8 coverage texture the renderer packs
//! rasterized glyph bitmaps into, plus the CPU backing it uploads from.
//!
//! Text layout ([`viso_text`]) owns shaping and per-glyph coverage bitmaps but
//! never touches a GPU texture — packing, UV assignment, dirty tracking, and
//! upload are the renderer's job (§16.1). This module is that seam: hand it a
//! [`CoverageBitmap`] and it returns the normalized UV sub-rect where the bitmap
//! now lives, having blitted the coverage into the CPU backing and grown the
//! accumulated dirty rect. The caller drains [`take_dirty`](GlyphAtlas::take_dirty)
//! once per frame and uploads exactly that sub-rect.
//!
//! # Representation
//!
//! One plane, [`TextureFormat::R8Unorm`]: the texel *is* exact per-pixel
//! coverage, sampled directly by the GlyphRun shader (`alpha = texel.r`). This
//! is the coverage lane; the scalable (MTSDF) and color lanes land later and do
//! not share this atlas.
//!
//! # Packing
//!
//! A max-rects free-rectangle allocator, best-short-side-fit: among the free
//! rectangles a glyph fits in, pick the one whose smaller leftover dimension is
//! smallest (ties broken by the larger leftover, then leftover area), so the
//! tightest pocket is consumed first and large open regions stay open. A
//! placement splits the intersecting free rects into the (up to four) L-shaped
//! remainders and prunes any remainder wholly contained in another. There is a
//! 1-texel gutter around every glyph so a bilinear sample never bleeds a
//! neighbor into a glyph's edge.
//!
//! # Overflow
//!
//! When a glyph does not fit, the atlas wipes generationally: the packer resets
//! to one empty free rect, the CPU backing is cleared, the whole texture is
//! marked dirty, and the [`epoch`](GlyphAtlas::epoch) bumps. Callers key their
//! per-glyph UV caches on the epoch and re-pack from scratch after a bump — no
//! incremental eviction. The caller re-inserts live glyphs; the just-rejected
//! glyph is retried once against the fresh atlas.

use viso_gpu::{TextureFormat, TextureId};
use viso_text::CoverageBitmap;

use crate::primitive::Rect;
use crate::rect_packer::{RectPacker, TexelRect};

/// A single-channel A8 coverage atlas: a packer, a CPU pixel backing, and the
/// accumulated dirty rect the renderer uploads.
///
/// The GPU [`TextureId`] is created once by the caller (the renderer owns
/// device resource creation) and handed in; this type never touches the device.
#[derive(Debug)]
pub struct GlyphAtlas {
    /// Atlas edge length in texels (square, `size × size`).
    size: u32,
    /// The free-rect packer.
    packer: RectPacker,
    /// Row-major R8 pixels, `size²` bytes; the CPU source of truth for upload.
    pixels: Vec<u8>,
    /// The GPU texture (`R8Unorm`, `size × size`) these pixels back.
    texture: TextureId,
    /// Accumulated dirty rect since the last [`take_dirty`](Self::take_dirty),
    /// or `None` when nothing changed.
    dirty: Option<TexelRect>,
    /// Generational epoch; bumped on every overflow wipe so callers invalidate
    /// UV caches keyed on it.
    epoch: u32,
}

/// The result of [`GlyphAtlas::alloc`].
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum AtlasAlloc {
    /// The glyph was packed at this normalized UV sub-rect.
    Placed(Rect),
    /// The glyph was empty (zero-area coverage, e.g. a space); nothing packed.
    Empty,
    /// The glyph did not fit; the atlas wiped generationally (epoch bumped) and
    /// the caller must re-pack live glyphs and retry this one.
    Overflow,
}

impl GlyphAtlas {
    /// A fresh atlas of `size × size` texels, backing the given GPU texture.
    ///
    /// The texture must be created as [`TextureFormat::R8Unorm`] at the same
    /// dimensions. The backing starts fully zero (transparent coverage).
    pub fn new(size: u32, texture: TextureId) -> Self {
        Self {
            size,
            packer: RectPacker::new(size),
            pixels: vec![0u8; (size as usize) * (size as usize)],
            texture,
            dirty: None,
            epoch: 0,
        }
    }

    /// The R8 pixel format an atlas texture must be created with.
    pub const FORMAT: TextureFormat = TextureFormat::R8Unorm;

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

    /// The full CPU pixel backing (row-major R8, `size²` bytes).
    pub fn pixels(&self) -> &[u8] {
        &self.pixels
    }

    /// Pack `bitmap`'s coverage into the atlas, returning where it landed.
    ///
    /// A zero-area bitmap (a space) is [`AtlasAlloc::Empty`] with no allocation.
    /// On success the coverage is blitted into the CPU backing (dirty rect
    /// grown) and the normalized UV sub-rect is returned. On overflow the atlas
    /// wipes generationally and returns [`AtlasAlloc::Overflow`]; the caller
    /// re-packs and retries.
    pub fn alloc(&mut self, bitmap: &CoverageBitmap) -> AtlasAlloc {
        if bitmap.is_empty() {
            return AtlasAlloc::Empty;
        }
        let gw = bitmap.width;
        let gh = bitmap.height;
        // 1-texel gutter on all sides so bilinear sampling never bleeds a
        // neighbor into this glyph's fringe.
        let pad_w = gw + 2;
        let pad_h = gh + 2;
        if pad_w > self.size || pad_h > self.size {
            // Larger than the atlas even alone: a wipe would not help, so this
            // is a hard reject reported as overflow (the caller decides).
            return AtlasAlloc::Overflow;
        }

        let (px, py) = match self.packer.allocate(pad_w, pad_h) {
            Some(origin) => origin,
            None => {
                self.wipe();
                return AtlasAlloc::Overflow;
            }
        };
        // The glyph sits one texel in from the padded cell's origin.
        let gx = px + 1;
        let gy = py + 1;
        self.blit(gx, gy, bitmap);
        self.grow_dirty(TexelRect {
            x: gx,
            y: gy,
            w: gw,
            h: gh,
        });

        let inv = 1.0 / self.size as f32;
        AtlasAlloc::Placed(Rect {
            x: gx as f32 * inv,
            y: gy as f32 * inv,
            w: gw as f32 * inv,
            h: gh as f32 * inv,
        })
    }

    /// Take the accumulated dirty sub-rect (in texels) and its bytes, resetting
    /// the dirty state. Returns `None` when nothing changed since the last call.
    ///
    /// The bytes are the tightly-packed rows of the dirty sub-rect, suitable for
    /// [`viso_gpu::GpuBackend::write_texture`] at `(x, y, w, h)`.
    pub fn take_dirty(&mut self) -> Option<(u32, u32, u32, u32, Vec<u8>)> {
        let d = self.dirty.take()?;
        let mut bytes = Vec::with_capacity((d.w * d.h) as usize);
        for row in 0..d.h {
            let start = ((d.y + row) * self.size + d.x) as usize;
            bytes.extend_from_slice(&self.pixels[start..start + d.w as usize]);
        }
        Some((d.x, d.y, d.w, d.h, bytes))
    }

    /// Blit a coverage bitmap into the CPU backing at texel `(x, y)`.
    fn blit(&mut self, x: u32, y: u32, bitmap: &CoverageBitmap) {
        for row in 0..bitmap.height {
            let src = (row * bitmap.width) as usize;
            let dst = ((y + row) * self.size + x) as usize;
            self.pixels[dst..dst + bitmap.width as usize]
                .copy_from_slice(&bitmap.coverage[src..src + bitmap.width as usize]);
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

    fn bitmap(w: u32, h: u32, fill: u8) -> CoverageBitmap {
        CoverageBitmap {
            width: w,
            height: h,
            left: 0.0,
            top: 0.0,
            coverage: vec![fill; (w * h) as usize],
        }
    }

    #[test]
    fn empty_bitmap_allocates_nothing() {
        let mut atlas = GlyphAtlas::new(64, TextureId(0));
        assert_eq!(atlas.alloc(&bitmap(0, 0, 0)), AtlasAlloc::Empty);
        assert!(atlas.take_dirty().is_none());
    }

    #[test]
    fn placed_uv_is_normalized_and_gutter_offset() {
        let mut atlas = GlyphAtlas::new(64, TextureId(0));
        let AtlasAlloc::Placed(uv) = atlas.alloc(&bitmap(8, 8, 200)) else {
            panic!("should place");
        };
        // First glyph lands one texel in from the origin (the gutter).
        assert_eq!(uv.x, 1.0 / 64.0);
        assert_eq!(uv.y, 1.0 / 64.0);
        assert_eq!(uv.w, 8.0 / 64.0);
        assert_eq!(uv.h, 8.0 / 64.0);
    }

    #[test]
    fn blit_writes_coverage_and_dirty_rect() {
        let mut atlas = GlyphAtlas::new(64, TextureId(0));
        atlas.alloc(&bitmap(4, 4, 123));
        let (x, y, w, h, bytes) = atlas.take_dirty().expect("dirty after alloc");
        assert_eq!((x, y, w, h), (1, 1, 4, 4));
        assert_eq!(bytes.len(), 16);
        assert!(bytes.iter().all(|&b| b == 123));
        // A second take is empty until another alloc.
        assert!(atlas.take_dirty().is_none());
    }

    #[test]
    fn two_glyphs_do_not_overlap() {
        let mut atlas = GlyphAtlas::new(64, TextureId(0));
        let AtlasAlloc::Placed(a) = atlas.alloc(&bitmap(10, 10, 50)) else {
            panic!()
        };
        let AtlasAlloc::Placed(b) = atlas.alloc(&bitmap(10, 10, 50)) else {
            panic!()
        };
        // Different sub-rects (best-fit puts the second one elsewhere).
        assert_ne!((a.x, a.y), (b.x, b.y));
    }

    #[test]
    fn overflow_wipes_and_bumps_epoch() {
        // A tiny atlas that fits one padded 8×8 (10×10) but not two.
        let mut atlas = GlyphAtlas::new(12, TextureId(0));
        assert!(matches!(
            atlas.alloc(&bitmap(8, 8, 9)),
            AtlasAlloc::Placed(_)
        ));
        assert_eq!(atlas.epoch(), 0);
        // Second does not fit → generational wipe.
        assert_eq!(atlas.alloc(&bitmap(8, 8, 9)), AtlasAlloc::Overflow);
        assert_eq!(atlas.epoch(), 1);
        // The whole texture is dirty after a wipe.
        let (x, y, w, h, _) = atlas.take_dirty().expect("wipe dirties all");
        assert_eq!((x, y, w, h), (0, 0, 12, 12));
        // Re-packing the rejected glyph now succeeds against the fresh atlas.
        assert!(matches!(
            atlas.alloc(&bitmap(8, 8, 9)),
            AtlasAlloc::Placed(_)
        ));
    }

    #[test]
    fn glyph_larger_than_atlas_is_overflow_without_wipe_loop() {
        let mut atlas = GlyphAtlas::new(8, TextureId(0));
        // 8×8 glyph needs 10×10 with the gutter — never fits an 8-texel atlas.
        assert_eq!(atlas.alloc(&bitmap(8, 8, 1)), AtlasAlloc::Overflow);
        // No wipe happened (epoch unchanged): a wipe would not help.
        assert_eq!(atlas.epoch(), 0);
    }
}
