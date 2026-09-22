//! The color-glyph atlas: an RGBA8 texture the renderer packs premultiplied
//! color-emoji bitmaps into, plus the CPU backing it uploads from.
//!
//! Structurally a sibling of [`GlyphAtlas`](crate::glyph_atlas::GlyphAtlas): the
//! same paged plane and page-granular residency lifecycle, but four bytes per
//! texel ([`TextureFormat::Rgba8Unorm`]) instead of one. Color glyphs
//! ([`ColorGlyph`]) carry their own premultiplied RGBA, so they lower through
//! the Image primitive with a white tint rather than the coverage shader; this
//! atlas is where those bitmaps live.
//!
//! Its pages are budgeted by the RGBA pool of the caller's residency, entirely
//! independently of the coverage atlas: filling this atlas evicts color pages and
//! nothing else.

use viso_gpu::{TextureFormat, TextureId};
use viso_text::system_fonts::ColorGlyph;

use crate::atlas_plane::AtlasPlane;
use crate::primitive::Rect;

/// Bytes per texel of the RGBA8 backing.
const BPT: u32 = 4;

/// An RGBA8 color-glyph atlas: a paged plane plus the GPU texture its pixels
/// back.
///
/// The GPU [`TextureId`] is created once by the caller (the renderer owns
/// device resource creation) and handed in; this type never touches the device.
#[derive(Debug)]
pub struct ColorAtlas {
    /// The paged pixel plane: per-page packers, CPU pixels, dirty rect.
    plane: AtlasPlane,
    /// The GPU texture (`Rgba8Unorm`, `size × size`) these pixels back.
    texture: TextureId,
}

/// The result of [`ColorAtlas::alloc_in_page`].
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum ColorAlloc {
    /// The glyph was packed at this normalized UV sub-rect.
    Placed(Rect),
    /// The glyph was empty (zero-area bitmap); nothing packed.
    Empty,
    /// The named page has no room left. The caller reclaims a page,
    /// [`reset_page`](ColorAtlas::reset_page)s it, and retries there; nothing was
    /// modified by this call.
    PageFull,
    /// The glyph is larger than a whole page — no eviction can make it fit.
    TooLarge,
}

impl ColorAtlas {
    /// A fresh atlas of `size × size` texels cut into `page_size × page_size`
    /// pages, backing the given GPU texture.
    ///
    /// The texture must be created as [`TextureFormat::Rgba8Unorm`] at the same
    /// dimensions. The backing starts fully zero (transparent).
    pub fn new(size: u32, page_size: u32, texture: TextureId) -> Self {
        Self {
            plane: AtlasPlane::new(size, page_size, BPT),
            texture,
        }
    }

    /// The RGBA8 pixel format a color-atlas texture must be created with.
    pub const FORMAT: TextureFormat = TextureFormat::Rgba8Unorm;

    /// Atlas edge length in texels.
    pub fn size(&self) -> u32 {
        self.plane.size()
    }

    /// Page edge length in texels.
    pub fn page_size(&self) -> u32 {
        self.plane.page_size()
    }

    /// How many pages this atlas holds — the page budget of the residency pool
    /// that drives it.
    pub fn page_count(&self) -> usize {
        self.plane.page_count()
    }

    /// The byte capacity of one page — the `page_bytes` of the residency pool
    /// that drives it.
    pub fn page_bytes(&self) -> usize {
        self.plane.page_bytes()
    }

    /// The GPU texture handle these pixels back.
    pub fn texture(&self) -> TextureId {
        self.texture
    }

    /// The full CPU pixel backing (row-major RGBA8, `size² × 4` bytes).
    pub fn pixels(&self) -> &[u8] {
        self.plane.pixels()
    }

    /// Pack `glyph`'s premultiplied RGBA into `page`, returning where it landed.
    ///
    /// A zero-area bitmap is [`ColorAlloc::Empty`] with no allocation. On success
    /// the RGBA is blitted into the CPU backing (dirty rect grown) and the
    /// normalized UV sub-rect is returned.
    pub fn alloc_in_page(&mut self, page: usize, glyph: &ColorGlyph) -> ColorAlloc {
        let gw = glyph.width;
        let gh = glyph.height;
        if gw == 0 || gh == 0 || glyph.rgba.is_empty() {
            return ColorAlloc::Empty;
        }
        // 1-texel gutter on all sides so bilinear sampling never bleeds a
        // neighbor — or the adjacent page — into this glyph's fringe.
        let (pad_w, pad_h) = (gw + 2, gh + 2);
        if self.plane.exceeds_page(pad_w, pad_h) {
            return ColorAlloc::TooLarge;
        }
        let Some((px, py)) = self.plane.place(page, pad_w, pad_h) else {
            return ColorAlloc::PageFull;
        };
        // The glyph sits one texel in from the padded cell's origin.
        let (gx, gy) = (px + 1, py + 1);
        self.plane.blit(gx, gy, gw, gh, &glyph.rgba);
        ColorAlloc::Placed(self.plane.uv(gx, gy, gw, gh))
    }

    /// Reopen `page` for packing after residency reclaimed it. Only that page's
    /// packer is reset; no pixels are cleared and no upload is triggered.
    pub fn reset_page(&mut self, page: usize) {
        self.plane.reset_page(page);
    }

    /// Take the accumulated dirty sub-rect (in texels) and its RGBA bytes,
    /// resetting the dirty state. Returns `None` when nothing changed.
    ///
    /// The bytes are the tightly-packed rows of the dirty sub-rect (`w × 4`
    /// bytes per row), suitable for
    /// [`viso_gpu::GpuBackend::write_texture`] at `(x, y, w, h)`.
    pub fn take_dirty(&mut self) -> Option<(u32, u32, u32, u32, Vec<u8>)> {
        self.plane.take_dirty()
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
        let mut atlas = ColorAtlas::new(64, 64, TextureId::new(0));
        assert_eq!(
            atlas.alloc_in_page(0, &glyph(0, 0, [0; 4])),
            ColorAlloc::Empty
        );
        assert!(atlas.take_dirty().is_none());
    }

    #[test]
    fn placed_uv_is_normalized_and_gutter_offset() {
        let mut atlas = ColorAtlas::new(64, 64, TextureId::new(0));
        let ColorAlloc::Placed(uv) = atlas.alloc_in_page(0, &glyph(8, 8, [10, 20, 30, 40])) else {
            panic!("should place");
        };
        assert_eq!(uv.x, 1.0 / 64.0);
        assert_eq!(uv.y, 1.0 / 64.0);
        assert_eq!(uv.w, 8.0 / 64.0);
        assert_eq!(uv.h, 8.0 / 64.0);
    }

    #[test]
    fn blit_writes_rgba_and_dirty_rect() {
        let mut atlas = ColorAtlas::new(64, 64, TextureId::new(0));
        atlas.alloc_in_page(0, &glyph(4, 4, [1, 2, 3, 4]));
        let (x, y, w, h, bytes) = atlas.take_dirty().expect("dirty after alloc");
        assert_eq!((x, y, w, h), (1, 1, 4, 4));
        assert_eq!(bytes.len(), 4 * 4 * 4);
        assert_eq!(&bytes[0..4], &[1, 2, 3, 4]);
        assert!(atlas.take_dirty().is_none());
    }

    #[test]
    fn a_full_page_reports_itself_and_destroys_nothing() {
        // Two pages of 12 texels: one fits a padded 8×8 (10×10), not two.
        let mut atlas = ColorAtlas::new(24, 12, TextureId::new(0));
        let ColorAlloc::Placed(first) = atlas.alloc_in_page(0, &glyph(8, 8, [9; 4])) else {
            panic!("first fits")
        };
        atlas.take_dirty();

        assert_eq!(
            atlas.alloc_in_page(0, &glyph(8, 8, [9; 4])),
            ColorAlloc::PageFull
        );
        assert!(
            atlas.take_dirty().is_none(),
            "a full page uploads nothing and clears nothing"
        );

        assert!(matches!(
            atlas.alloc_in_page(1, &glyph(8, 8, [9; 4])),
            ColorAlloc::Placed(_)
        ));
        atlas.reset_page(0);
        let ColorAlloc::Placed(again) = atlas.alloc_in_page(0, &glyph(8, 8, [9; 4])) else {
            panic!("page 0 reopened")
        };
        assert_eq!((first.x, first.y), (again.x, again.y));
    }

    #[test]
    fn glyph_larger_than_a_page_is_too_large() {
        let mut atlas = ColorAtlas::new(64, 8, TextureId::new(0));
        assert_eq!(
            atlas.alloc_in_page(0, &glyph(8, 8, [1; 4])),
            ColorAlloc::TooLarge
        );
        assert!(atlas.take_dirty().is_none());
    }

    #[test]
    fn page_geometry_matches_the_residency_budget() {
        let atlas = ColorAtlas::new(1024, 256, TextureId::new(0));
        assert_eq!(atlas.page_count(), 16);
        assert_eq!(atlas.page_bytes(), 256 * 256 * 4);
    }
}
