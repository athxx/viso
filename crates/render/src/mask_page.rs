//! The physical mask page: a single-channel R8 coverage texture the renderer
//! blits resolved clip/mask coverage into, plus the CPU backing it uploads from.
//!
//! This is the storage half of the mask model (§14.4). [`MaskCache`](crate::mask)
//! owns the *policy* — packing ROIs into the page, keying so a stable mask is
//! built once, and reclaiming slots — and hands back a [`MaskSlot`] naming where
//! a coverage bitmap belongs. `MaskPage` owns the *physical* side: the GPU
//! texture, the CPU-side pixel backing, and the accumulated dirty rect. Give it a
//! slot and a coverage bitmap and it copies the coverage into the backing at the
//! slot origin and grows the dirty rect; the caller drains
//! [`take_dirty`](MaskPage::take_dirty) once per frame and uploads exactly that
//! sub-rect, mirroring the glyph atlas and gradient LUT upload seams.
//!
//! One plane, [`TextureFormat::R8Unorm`]: the texel *is* per-pixel coverage,
//! sampled directly by the reused GlyphRun fragment (`alpha = color.a * texel.r`)
//! — a masked solid fill is one coverage texture times a constant color, exactly
//! the glyph fragment, so no new pipeline or shader is needed.
//!
//! Repacking lives in [`MaskCache::end_frame`](crate::mask::MaskCache::end_frame):
//! when it evicts and repacks survivors, slot origins can move, so it returns a
//! "page was repacked" signal the renderer stores as
//! [`needs_full_reblit`](MaskPage::needs_full_reblit) and honors by re-blitting
//! every resolving mask the next frame regardless of cache-hit status.

use viso_gpu::{TextureFormat, TextureId};

use crate::mask::MaskSlot;
use crate::rect_packer::TexelRect;

/// A single-channel R8 mask page: the GPU texture, its CPU pixel backing, and
/// the accumulated dirty sub-rect awaiting upload.
#[derive(Debug)]
pub struct MaskPage {
    /// Square page edge in texels.
    size: u32,
    /// CPU backing, `size * size` bytes, row-major, one byte of coverage per
    /// texel. The upload source of truth.
    pixels: Vec<u8>,
    /// The GPU texture this page uploads into.
    texture: TextureId,
    /// The accumulated dirty sub-rect since the last drain, or `None` when the
    /// backing matches the texture.
    dirty: Option<TexelRect>,
    /// Set by the renderer from [`MaskCache::end_frame`](crate::mask::MaskCache::end_frame):
    /// the cache repacked and slot origins may have moved, so every resolving
    /// mask must be re-blitted next frame regardless of cache-hit status.
    needs_full_reblit: bool,
}

impl MaskPage {
    /// The page's texture format: single-channel 8-bit coverage.
    pub const FORMAT: TextureFormat = TextureFormat::R8Unorm;

    /// A fresh page of `size * size` texels backing `texture`, all-zero (no
    /// coverage) and clean.
    pub fn new(size: u32, texture: TextureId) -> Self {
        Self {
            size,
            pixels: vec![0u8; (size as usize) * (size as usize)],
            texture,
            dirty: None,
            needs_full_reblit: false,
        }
    }

    /// The page edge in texels.
    pub fn size(&self) -> u32 {
        self.size
    }

    /// The GPU texture this page uploads into.
    pub fn texture(&self) -> TextureId {
        self.texture
    }

    /// Whether the last cache repack moved slot origins, so every resolving mask
    /// must be re-blitted this frame.
    pub fn needs_full_reblit(&self) -> bool {
        self.needs_full_reblit
    }

    /// Record whether the next frame must re-blit every resolving mask (set from
    /// [`MaskCache::end_frame`](crate::mask::MaskCache::end_frame)'s repack signal).
    pub fn set_needs_full_reblit(&mut self, needs: bool) {
        self.needs_full_reblit = needs;
    }

    /// Blit `coverage` (row-major, `slot.w * slot.h` bytes) into the CPU backing
    /// at the slot origin and grow the accumulated dirty rect.
    ///
    /// A mismatched or out-of-bounds coverage buffer is ignored: the cache sizes
    /// the slot to the ROI and the rasterizer emits exactly that many bytes, so a
    /// mismatch is a caller bug, not silent corruption of the page.
    pub fn blit(&mut self, slot: MaskSlot, coverage: &[u8]) {
        let (x, y, w, h) = (slot.x, slot.y, slot.w, slot.h);
        if w == 0 || h == 0 {
            return;
        }
        if x + w > self.size || y + h > self.size {
            return;
        }
        if coverage.len() != (w as usize) * (h as usize) {
            return;
        }
        for row in 0..h {
            let dst = ((y + row) * self.size + x) as usize;
            let src = (row * w) as usize;
            self.pixels[dst..dst + w as usize].copy_from_slice(&coverage[src..src + w as usize]);
        }
        self.grow_dirty(TexelRect { x, y, w, h });
    }

    /// Drain the accumulated dirty rect: its origin/extent and a tightly-packed
    /// copy of that sub-rect's texels, or `None` when nothing changed.
    ///
    /// Signature matches [`GlyphAtlas::take_dirty`](crate::glyph_atlas::GlyphAtlas::take_dirty)
    /// so the renderer drains both the same way — one batched `write_texture` per
    /// frame.
    pub fn take_dirty(&mut self) -> Option<(u32, u32, u32, u32, Vec<u8>)> {
        let d = self.dirty.take()?;
        let mut bytes = Vec::with_capacity((d.w * d.h) as usize);
        for row in 0..d.h {
            let start = ((d.y + row) * self.size + d.x) as usize;
            bytes.extend_from_slice(&self.pixels[start..start + d.w as usize]);
        }
        Some((d.x, d.y, d.w, d.h, bytes))
    }

    /// Clear the page: zero the backing and mark the whole texture dirty so the
    /// next drain re-uploads it. Paired with a [`MaskCache`](crate::mask) reset.
    pub fn wipe(&mut self) {
        self.pixels.iter_mut().for_each(|p| *p = 0);
        self.dirty = Some(TexelRect {
            x: 0,
            y: 0,
            w: self.size,
            h: self.size,
        });
        self.needs_full_reblit = true;
    }

    /// Union a just-written sub-rect into the accumulated dirty rect.
    fn grow_dirty(&mut self, r: TexelRect) {
        self.dirty = Some(match self.dirty.take() {
            None => r,
            Some(cur) => {
                let x = cur.x.min(r.x);
                let y = cur.y.min(r.y);
                let max_x = cur.max_x().max(r.max_x());
                let max_y = cur.max_y().max(r.max_y());
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
    use crate::mask::MaskFormat;

    fn slot(x: u32, y: u32, w: u32, h: u32) -> MaskSlot {
        MaskSlot {
            format: MaskFormat::R8,
            x,
            y,
            w,
            h,
        }
    }

    /// A blit copies coverage into the backing and take_dirty returns exactly the
    /// written sub-rect's bytes.
    #[test]
    fn blit_then_drain_returns_written_subrect() {
        let mut page = MaskPage::new(16, TextureId::new(1));
        let cov = vec![7u8; 2 * 2];
        page.blit(slot(3, 4, 2, 2), &cov);
        let (x, y, w, h, bytes) = page.take_dirty().expect("dirty after blit");
        assert_eq!((x, y, w, h), (3, 4, 2, 2));
        assert_eq!(bytes, vec![7u8; 4]);
        // Drained: nothing left dirty.
        assert!(page.take_dirty().is_none());
    }

    /// Two disjoint blits coalesce into one covering dirty rect.
    #[test]
    fn disjoint_blits_coalesce_dirty_rect() {
        let mut page = MaskPage::new(16, TextureId::new(1));
        page.blit(slot(0, 0, 1, 1), &[1]);
        page.blit(slot(5, 6, 1, 1), &[2]);
        let (x, y, w, h, _) = page.take_dirty().expect("dirty");
        assert_eq!((x, y), (0, 0));
        assert_eq!((w, h), (6, 7));
    }

    /// A mismatched coverage length is ignored rather than corrupting the page.
    #[test]
    fn mismatched_coverage_is_ignored() {
        let mut page = MaskPage::new(16, TextureId::new(1));
        page.blit(slot(0, 0, 2, 2), &[1, 2, 3]);
        assert!(page.take_dirty().is_none());
    }

    /// wipe zeroes the backing, marks the whole texture dirty, and forces a full
    /// re-blit.
    #[test]
    fn wipe_marks_full_texture_dirty() {
        let mut page = MaskPage::new(8, TextureId::new(1));
        page.blit(slot(1, 1, 2, 2), &[9; 4]);
        page.wipe();
        assert!(page.needs_full_reblit());
        let (x, y, w, h, bytes) = page.take_dirty().expect("dirty after wipe");
        assert_eq!((x, y, w, h), (0, 0, 8, 8));
        assert!(bytes.iter().all(|&b| b == 0));
    }

    /// The repack signal round-trips through the page.
    #[test]
    fn reblit_flag_round_trips() {
        let mut page = MaskPage::new(8, TextureId::new(1));
        assert!(!page.needs_full_reblit());
        page.set_needs_full_reblit(true);
        assert!(page.needs_full_reblit());
    }
}
