//! The paged atlas plane shared by the coverage and color atlases: a square
//! texture cut into equal square pages, one rectangle packer per page, the CPU
//! pixel backing, and the accumulated dirty rect.
//!
//! The page is the unit of residency (`viso_text::GlyphResidency` owns which
//! glyph lives on which page); this type owns the *pixels* of those pages. The
//! two responsibilities meet at exactly two operations: place a glyph on a named
//! page, and reset a named page because residency reclaimed it.
//!
//! # A reset page is not a cleared atlas
//!
//! [`Self::reset_page`] resets that page's packer and nothing else. No other
//! page's placements move, no pixels are zeroed, and no dirty rect is grown — the
//! stale texels are simply unreachable, because the glyph placements that pointed
//! at them are dropped in the same step by the caller, and the next glyph
//! admitted onto the page overwrites them. A full atlas is therefore a steady
//! state: pages turn over one at a time and a frame uploads only what it packed.
//!
//! # Pages are independently packed, with a gutter at every edge
//!
//! Each page holds its own [`RectPacker`] over page-local texels, so packing one
//! page never inspects or fragments another. Callers pad every box by a 1-texel
//! gutter, which also keeps a glyph one texel inside its page — so two glyphs on
//! adjacent pages are at least two texels apart and a bilinear sample can never
//! bleed across a page boundary.

use crate::primitive::Rect;
use crate::rect_packer::{RectPacker, TexelRect};

/// A square atlas plane divided into square pages, with a CPU pixel backing of
/// `bytes_per_texel` bytes per texel.
#[derive(Debug)]
pub struct AtlasPlane {
    /// Atlas edge length in texels (square, `size × size`).
    size: u32,
    /// Page edge length in texels; `size` is a whole multiple of it.
    page_size: u32,
    /// Pages along one axis (`size / page_size`).
    per_axis: u32,
    /// Bytes per texel of the backing (1 for A8 coverage, 4 for RGBA8).
    bytes_per_texel: u32,
    /// One packer per page, in page-index order, over page-local texels.
    packers: Vec<RectPacker>,
    /// Row-major pixels, `size² × bytes_per_texel` bytes.
    pixels: Vec<u8>,
    /// Accumulated dirty rect since the last [`Self::take_dirty`].
    dirty: Option<TexelRect>,
}

impl AtlasPlane {
    /// A plane of `size × size` texels cut into `page_size × page_size` pages.
    ///
    /// `page_size` is clamped to `size` and rounded down to a divisor-friendly
    /// value by integer division, so the pages always tile the plane exactly and
    /// the trailing texels (when `size` is not a multiple) stay unused rather
    /// than half-owned.
    pub fn new(size: u32, page_size: u32, bytes_per_texel: u32) -> Self {
        let page_size = page_size.clamp(1, size.max(1));
        let per_axis = (size / page_size).max(1);
        let pages = (per_axis * per_axis) as usize;
        Self {
            size,
            page_size,
            per_axis,
            bytes_per_texel,
            packers: (0..pages).map(|_| RectPacker::new(page_size)).collect(),
            pixels: vec![0u8; (size as usize) * (size as usize) * bytes_per_texel as usize],
            dirty: None,
        }
    }

    /// Atlas edge length in texels.
    pub fn size(&self) -> u32 {
        self.size
    }

    /// Page edge length in texels.
    pub fn page_size(&self) -> u32 {
        self.page_size
    }

    /// How many pages this plane holds — the page budget a residency pool over
    /// this plane must be given.
    pub fn page_count(&self) -> usize {
        self.packers.len()
    }

    /// The byte capacity of one page: its texels times bytes per texel. This is
    /// the `page_bytes` a residency pool over this plane must be budgeted with.
    pub fn page_bytes(&self) -> usize {
        (self.page_size as usize) * (self.page_size as usize) * self.bytes_per_texel as usize
    }

    /// The full CPU pixel backing (row-major, `size² × bytes_per_texel` bytes).
    pub fn pixels(&self) -> &[u8] {
        &self.pixels
    }

    /// Whether a `w × h` box can never fit any page of this plane, even empty.
    pub fn exceeds_page(&self, w: u32, h: u32) -> bool {
        w > self.page_size || h > self.page_size
    }

    /// Place a `w × h` box on `page`, returning its atlas-space texel origin, or
    /// `None` when that page has no room left for it.
    pub fn place(&mut self, page: usize, w: u32, h: u32) -> Option<(u32, u32)> {
        let (ox, oy) = self.page_origin(page)?;
        let (lx, ly) = self.packers.get_mut(page)?.allocate(w, h)?;
        Some((ox + lx, oy + ly))
    }

    /// Reopen `page` for packing after residency reclaimed it.
    ///
    /// Only this page's packer is reset: other pages keep their placements, the
    /// pixels are left as they are (the next glyph overwrites them), and no
    /// upload is triggered.
    pub fn reset_page(&mut self, page: usize) {
        if let Some(packer) = self.packers.get_mut(page) {
            packer.reset();
        }
    }

    /// Blit `w × h` texels of tightly-packed `src` into the backing at atlas
    /// texel `(x, y)` and grow the dirty rect to cover them.
    pub fn blit(&mut self, x: u32, y: u32, w: u32, h: u32, src: &[u8]) {
        let bpt = self.bytes_per_texel as usize;
        let row_bytes = w as usize * bpt;
        for row in 0..h {
            let from = row as usize * row_bytes;
            let to = ((y + row) as usize * self.size as usize + x as usize) * bpt;
            self.pixels[to..to + row_bytes].copy_from_slice(&src[from..from + row_bytes]);
        }
        self.grow_dirty(TexelRect { x, y, w, h });
    }

    /// The normalized UV sub-rect of an atlas texel rect.
    pub fn uv(&self, x: u32, y: u32, w: u32, h: u32) -> Rect {
        let inv = 1.0 / self.size as f32;
        Rect {
            x: x as f32 * inv,
            y: y as f32 * inv,
            w: w as f32 * inv,
            h: h as f32 * inv,
        }
    }

    /// Take the accumulated dirty sub-rect and its tightly-packed rows,
    /// resetting the dirty state. `None` when nothing changed since the last
    /// call — the steady-state answer for a warm working set.
    pub fn take_dirty(&mut self) -> Option<(u32, u32, u32, u32, Vec<u8>)> {
        let d = self.dirty.take()?;
        let bpt = self.bytes_per_texel as usize;
        let row_bytes = d.w as usize * bpt;
        let mut bytes = Vec::with_capacity(row_bytes * d.h as usize);
        for row in 0..d.h {
            let start = ((d.y + row) as usize * self.size as usize + d.x as usize) * bpt;
            bytes.extend_from_slice(&self.pixels[start..start + row_bytes]);
        }
        Some((d.x, d.y, d.w, d.h, bytes))
    }

    /// The atlas-space origin of a page, or `None` for an out-of-range index.
    fn page_origin(&self, page: usize) -> Option<(u32, u32)> {
        if page >= self.packers.len() {
            return None;
        }
        let page = page as u32;
        Some((
            (page % self.per_axis) * self.page_size,
            (page / self.per_axis) * self.page_size,
        ))
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
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pages_tile_the_plane_and_report_their_capacity() {
        let plane = AtlasPlane::new(64, 16, 1);
        assert_eq!(plane.page_count(), 16);
        assert_eq!(plane.page_bytes(), 256);
        assert_eq!(plane.page_origin(0), Some((0, 0)));
        assert_eq!(plane.page_origin(1), Some((16, 0)));
        assert_eq!(plane.page_origin(4), Some((0, 16)));
        assert_eq!(plane.page_origin(15), Some((48, 48)));
        assert_eq!(plane.page_origin(16), None);
    }

    #[test]
    fn a_page_byte_capacity_follows_bytes_per_texel() {
        assert_eq!(AtlasPlane::new(64, 16, 4).page_bytes(), 16 * 16 * 4);
    }

    #[test]
    fn placements_stay_inside_their_own_page() {
        let mut plane = AtlasPlane::new(64, 16, 1);
        let (x, y) = plane.place(5, 8, 8).expect("fits");
        // Page 5 is column 1, row 1.
        assert!((16..32).contains(&x) && (16..32).contains(&y));
        // Filling page 5 leaves every other page fully open.
        assert!(plane.place(5, 16, 16).is_none());
        assert!(plane.place(6, 16, 16).is_some());
    }

    #[test]
    fn resetting_a_page_reopens_only_that_page() {
        let mut plane = AtlasPlane::new(32, 16, 1);
        assert!(plane.place(0, 16, 16).is_some());
        assert!(plane.place(1, 16, 16).is_some());
        assert!(plane.place(0, 4, 4).is_none());

        plane.reset_page(0);
        assert!(plane.place(0, 16, 16).is_some(), "page 0 reopened");
        assert!(plane.place(1, 4, 4).is_none(), "page 1 was not reset");
    }

    #[test]
    fn a_reset_page_dirties_nothing_and_uploads_nothing() {
        let mut plane = AtlasPlane::new(32, 16, 1);
        let (x, y) = plane.place(0, 4, 4).expect("fits");
        plane.blit(x, y, 4, 4, &[7u8; 16]);
        assert!(plane.take_dirty().is_some());

        plane.reset_page(0);
        assert!(
            plane.take_dirty().is_none(),
            "a reclaim uploads nothing: no clear, no whole-texture dirty"
        );
    }

    #[test]
    fn a_blit_dirties_exactly_its_rows() {
        let mut plane = AtlasPlane::new(32, 32, 1);
        plane.blit(3, 4, 2, 2, &[9u8; 4]);
        let (x, y, w, h, bytes) = plane.take_dirty().expect("dirty");
        assert_eq!((x, y, w, h), (3, 4, 2, 2));
        assert_eq!(bytes, vec![9u8; 4]);
        assert!(plane.take_dirty().is_none());
    }

    #[test]
    fn rgba_rows_are_four_bytes_per_texel() {
        let mut plane = AtlasPlane::new(8, 8, 4);
        let src: Vec<u8> = (0..16u8).collect();
        plane.blit(1, 1, 2, 2, &src);
        let (_, _, w, h, bytes) = plane.take_dirty().expect("dirty");
        assert_eq!((w, h), (2, 2));
        assert_eq!(bytes, src);
    }

    #[test]
    fn a_box_larger_than_a_page_never_fits() {
        let plane = AtlasPlane::new(64, 16, 1);
        assert!(plane.exceeds_page(17, 4));
        assert!(!plane.exceeds_page(16, 16));
    }
}
