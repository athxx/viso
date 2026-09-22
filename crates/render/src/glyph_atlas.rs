//! The glyph atlas: a single-channel A8 coverage texture the renderer packs
//! rasterized glyph bitmaps into, plus the CPU backing it uploads from.
//!
//! Text layout ([`viso_text`]) owns shaping, per-glyph coverage bitmaps, and
//! *residency metadata* — which glyph lives on which page, and which page is
//! coldest — but never touches a GPU texture. Packing, UV assignment, dirty
//! tracking, and upload are the renderer's job (§16.1). This module is that seam:
//! name a page and hand it a [`CoverageBitmap`], and it returns the normalized UV
//! sub-rect where the bitmap now lives.
//!
//! # Representation
//!
//! One plane, [`TextureFormat::R8Unorm`]: the texel *is* exact per-pixel
//! coverage, sampled directly by the GlyphRun shader (`alpha = texel.r`). This
//! is the coverage lane; the scalable (MTSDF) and color lanes have their own
//! planes and do not share this atlas.
//!
//! # Packing
//!
//! The plane is cut into equal square pages, each with its own max-rects
//! free-rectangle allocator, best-short-side-fit: among the free rectangles a
//! glyph fits in, pick the one whose smaller leftover dimension is smallest, so
//! the tightest pocket is consumed first and large open regions stay open. There
//! is a 1-texel gutter around every glyph, which also keeps it one texel inside
//! its page, so a bilinear sample never bleeds a neighbor — or a neighboring
//! page — into a glyph's edge.
//!
//! # A full page is a steady state, not a reset
//!
//! When a page has no room, [`AtlasAlloc::PageFull`] says so and nothing is
//! destroyed. The caller (which owns residency) reclaims the coldest page, calls
//! [`reset_page`](GlyphAtlas::reset_page), drops the UVs that pointed into it,
//! and retries on that page. Live glyphs on every other page keep their pixels
//! and their UVs, and a frame uploads only what it actually packed — there is no
//! generational wipe and no whole-atlas re-upload. A glyph too large for any page
//! is [`AtlasAlloc::TooLarge`]: eviction would not help, so the caller must fall
//! back rather than retry.

use viso_gpu::{TextureFormat, TextureId};
use viso_text::CoverageBitmap;

use crate::atlas_plane::AtlasPlane;
use crate::primitive::Rect;

/// A single-channel A8 coverage atlas: a paged plane plus the GPU texture its
/// pixels back.
///
/// The GPU [`TextureId`] is created once by the caller (the renderer owns
/// device resource creation) and handed in; this type never touches the device.
#[derive(Debug)]
pub struct GlyphAtlas {
    /// The paged pixel plane: per-page packers, CPU pixels, dirty rect.
    plane: AtlasPlane,
    /// The GPU texture (`R8Unorm`, `size × size`) these pixels back.
    texture: TextureId,
}

/// The result of [`GlyphAtlas::alloc_in_page`].
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum AtlasAlloc {
    /// The glyph was packed at this normalized UV sub-rect.
    Placed(Rect),
    /// The glyph was empty (zero-area coverage, e.g. a space); nothing packed.
    Empty,
    /// The named page has no room left. The caller reclaims a page,
    /// [`reset_page`](GlyphAtlas::reset_page)s it, and retries there; nothing
    /// was modified by this call.
    PageFull,
    /// The glyph is larger than a whole page — no eviction can make it fit, so
    /// the caller must fall back to another representation.
    TooLarge,
}

impl GlyphAtlas {
    /// A fresh atlas of `size × size` texels cut into `page_size × page_size`
    /// pages, backing the given GPU texture.
    ///
    /// The texture must be created as [`TextureFormat::R8Unorm`] at the same
    /// dimensions. The backing starts fully zero (transparent coverage).
    pub fn new(size: u32, page_size: u32, texture: TextureId) -> Self {
        Self {
            plane: AtlasPlane::new(size, page_size, 1),
            texture,
        }
    }

    /// The R8 pixel format an atlas texture must be created with.
    pub const FORMAT: TextureFormat = TextureFormat::R8Unorm;

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

    /// The full CPU pixel backing (row-major R8, `size²` bytes).
    pub fn pixels(&self) -> &[u8] {
        self.plane.pixels()
    }

    /// Pack `bitmap`'s coverage into `page`, returning where it landed.
    ///
    /// A zero-area bitmap (a space) is [`AtlasAlloc::Empty`] with no allocation.
    /// On success the coverage is blitted into the CPU backing (dirty rect
    /// grown) and the normalized UV sub-rect is returned.
    pub fn alloc_in_page(&mut self, page: usize, bitmap: &CoverageBitmap) -> AtlasAlloc {
        if bitmap.is_empty() {
            return AtlasAlloc::Empty;
        }
        let gw = bitmap.width;
        let gh = bitmap.height;
        // 1-texel gutter on all sides so bilinear sampling never bleeds a
        // neighbor — or the adjacent page — into this glyph's fringe.
        let (pad_w, pad_h) = (gw + 2, gh + 2);
        if self.plane.exceeds_page(pad_w, pad_h) {
            return AtlasAlloc::TooLarge;
        }
        let Some((px, py)) = self.plane.place(page, pad_w, pad_h) else {
            return AtlasAlloc::PageFull;
        };
        // The glyph sits one texel in from the padded cell's origin.
        let (gx, gy) = (px + 1, py + 1);
        self.plane.blit(gx, gy, gw, gh, &bitmap.coverage);
        AtlasAlloc::Placed(self.plane.uv(gx, gy, gw, gh))
    }

    /// Reopen `page` for packing after residency reclaimed it. Only that page's
    /// packer is reset; no pixels are cleared and no upload is triggered.
    pub fn reset_page(&mut self, page: usize) {
        self.plane.reset_page(page);
    }

    /// Take the accumulated dirty sub-rect (in texels) and its bytes, resetting
    /// the dirty state. Returns `None` when nothing changed since the last call
    /// — the steady-state answer for a warm working set.
    ///
    /// The bytes are the tightly-packed rows of the dirty sub-rect, suitable for
    /// [`viso_gpu::GpuBackend::write_texture`] at `(x, y, w, h)`.
    pub fn take_dirty(&mut self) -> Option<(u32, u32, u32, u32, Vec<u8>)> {
        self.plane.take_dirty()
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
        let mut atlas = GlyphAtlas::new(64, 64, TextureId::new(0));
        assert_eq!(atlas.alloc_in_page(0, &bitmap(0, 0, 0)), AtlasAlloc::Empty);
        assert!(atlas.take_dirty().is_none());
    }

    #[test]
    fn placed_uv_is_normalized_and_gutter_offset() {
        let mut atlas = GlyphAtlas::new(64, 64, TextureId::new(0));
        let AtlasAlloc::Placed(uv) = atlas.alloc_in_page(0, &bitmap(8, 8, 200)) else {
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
        let mut atlas = GlyphAtlas::new(64, 64, TextureId::new(0));
        atlas.alloc_in_page(0, &bitmap(4, 4, 123));
        let (x, y, w, h, bytes) = atlas.take_dirty().expect("dirty after alloc");
        assert_eq!((x, y, w, h), (1, 1, 4, 4));
        assert_eq!(bytes.len(), 16);
        assert!(bytes.iter().all(|&b| b == 123));
        // A second take is empty until another alloc.
        assert!(atlas.take_dirty().is_none());
    }

    #[test]
    fn two_glyphs_do_not_overlap() {
        let mut atlas = GlyphAtlas::new(64, 64, TextureId::new(0));
        let AtlasAlloc::Placed(a) = atlas.alloc_in_page(0, &bitmap(10, 10, 50)) else {
            panic!()
        };
        let AtlasAlloc::Placed(b) = atlas.alloc_in_page(0, &bitmap(10, 10, 50)) else {
            panic!()
        };
        // Different sub-rects (best-fit puts the second one elsewhere).
        assert_ne!((a.x, a.y), (b.x, b.y));
    }

    #[test]
    fn a_full_page_reports_itself_and_destroys_nothing() {
        // Two pages of 12 texels: one fits a padded 8×8 (10×10), not two.
        let mut atlas = GlyphAtlas::new(24, 12, TextureId::new(0));
        let AtlasAlloc::Placed(first) = atlas.alloc_in_page(0, &bitmap(8, 8, 9)) else {
            panic!("first fits")
        };
        atlas.take_dirty();

        assert_eq!(
            atlas.alloc_in_page(0, &bitmap(8, 8, 9)),
            AtlasAlloc::PageFull
        );
        assert!(
            atlas.take_dirty().is_none(),
            "a full page uploads nothing and clears nothing"
        );

        // Another page takes it without disturbing page 0.
        assert!(matches!(
            atlas.alloc_in_page(1, &bitmap(8, 8, 9)),
            AtlasAlloc::Placed(_)
        ));
        // Reclaiming page 0 reopens exactly that page, at the same UV.
        atlas.reset_page(0);
        let AtlasAlloc::Placed(again) = atlas.alloc_in_page(0, &bitmap(8, 8, 9)) else {
            panic!("page 0 reopened")
        };
        assert_eq!((first.x, first.y), (again.x, again.y));
    }

    #[test]
    fn glyph_larger_than_a_page_is_too_large() {
        let mut atlas = GlyphAtlas::new(64, 8, TextureId::new(0));
        // 8×8 glyph needs 10×10 with the gutter — never fits an 8-texel page.
        assert_eq!(
            atlas.alloc_in_page(0, &bitmap(8, 8, 1)),
            AtlasAlloc::TooLarge
        );
        assert!(atlas.take_dirty().is_none());
    }

    #[test]
    fn page_geometry_matches_the_residency_budget() {
        let atlas = GlyphAtlas::new(1024, 256, TextureId::new(0));
        assert_eq!(atlas.page_count(), 16);
        assert_eq!(atlas.page_bytes(), 256 * 256);
        assert_eq!(atlas.page_size(), 256);
        assert_eq!(atlas.size(), 1024);
    }
}
