//! The physical mask pages: one single-channel R8 coverage texture per
//! [`MaskCache`](crate::mask::MaskCache) page, and the coverage written since
//! the last upload.
//!
//! This is the storage half of the mask model. The cache owns the
//! *policy* — which pages exist, packing ROIs into them, keying so a stable
//! mask is built once, and reclaiming slots — and hands back a [`MaskSlot`]
//! naming where a coverage bitmap belongs. `MaskPages` owns the *device*
//! side: it follows the cache's page list ([`sync`](MaskPages::sync)), keeps
//! each blitted bitmap until [`flush`](MaskPages::flush) writes it into its
//! slot, and holds no copy of the pages: a slot is only ever rewritten whole.
//!
//! One plane, [`TextureFormat::R8Unorm`]: the texel *is* per-pixel coverage,
//! sampled directly by the reused GlyphRun fragment (`alpha = color.a * texel.r`)
//! — a masked solid fill is one coverage texture times a constant color, exactly
//! the glyph fragment, so no new pipeline or shader is needed.
//!
//! Repacking lives in [`MaskCache::end_frame`](crate::mask::MaskCache::end_frame):
//! when it evicts and repacks survivors, slot origins can move, so it returns a
//! "pages were repacked" signal the renderer stores as
//! [`needs_full_reblit`](MaskPages::needs_full_reblit) and honors by re-blitting
//! every resolving mask the next frame regardless of cache-hit status.

use viso_gpu::{GpuBackend, TextureDesc, TextureFormat, TextureId};

use crate::mask::MaskSlot;

/// The bytes of zeros a new page is cleared with per write.
const CLEAR_BAND: usize = 64 * 1024;

/// The device textures behind the mask cache's pages.
#[derive(Debug, Default)]
pub struct MaskPages {
    /// Each page's texture and edge, in page order.
    pages: Vec<(TextureId, u32)>,
    /// Coverage blitted since the last flush: its slot and `w * h` texels.
    pending: Vec<(MaskSlot, Vec<u8>)>,
    /// Set by the renderer from [`MaskCache::end_frame`](crate::mask::MaskCache::end_frame):
    /// the cache repacked and slot origins may have moved, so every resolving
    /// mask must be re-blitted next frame regardless of cache-hit status.
    needs_full_reblit: bool,
}

impl MaskPages {
    /// The pages' texture format: single-channel 8-bit coverage.
    pub const FORMAT: TextureFormat = TextureFormat::R8Unorm;

    /// Match the textures to the cache's page `edges`: a page that is new or
    /// changed size gets a cleared texture, one the cache released gives its
    /// texture back. A no-op while the page list holds still.
    pub fn sync<B: GpuBackend>(
        &mut self,
        backend: &mut B,
        edges: impl ExactSizeIterator<Item = u32>,
    ) {
        let count = edges.len();
        for (index, edge) in edges.enumerate() {
            match self.pages.get(index) {
                Some(&(_, have)) if have == edge => continue,
                Some(&(texture, _)) => backend.destroy_texture(texture),
                None => {}
            }
            let texture = cleared_page(backend, edge);
            match self.pages.get_mut(index) {
                Some(page) => *page = (texture, edge),
                None => self.pages.push((texture, edge)),
            }
            self.pending
                .retain(|(slot, _)| usize::from(slot.page) != index);
        }
        for (texture, _) in self.pages.drain(count.min(self.pages.len())..) {
            backend.destroy_texture(texture);
        }
        self.pending
            .retain(|(slot, _)| usize::from(slot.page) < count);
    }

    /// Page `page`'s texture. Panics for a page [`sync`](Self::sync) has not
    /// created.
    pub fn texture(&self, page: u16) -> TextureId {
        self.pages[usize::from(page)].0
    }

    /// Page `page`'s edge in texels. Panics for a page
    /// [`sync`](Self::sync) has not created.
    pub fn edge(&self, page: u16) -> u32 {
        self.pages[usize::from(page)].1
    }

    /// Whether `texture` is one of the pages.
    pub fn contains(&self, texture: TextureId) -> bool {
        self.pages.iter().any(|&(t, _)| t == texture)
    }

    /// The device bytes the pages hold.
    pub fn bytes(&self) -> u64 {
        self.pages
            .iter()
            .map(|&(_, edge)| u64::from(edge) * u64::from(edge))
            .sum()
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

    /// Queue `coverage` (row-major, `slot.w * slot.h` bytes) for the slot.
    ///
    /// A mismatched or out-of-bounds coverage buffer is ignored: the cache sizes
    /// the slot to the ROI and the rasterizer emits exactly that many bytes, so a
    /// mismatch is a caller bug, not silent corruption of the page.
    pub fn blit(&mut self, slot: MaskSlot, coverage: Vec<u8>) {
        let Some(&(_, edge)) = self.pages.get(usize::from(slot.page)) else {
            return;
        };
        if slot.w == 0 || slot.h == 0 || slot.x + slot.w > edge || slot.y + slot.h > edge {
            return;
        }
        if coverage.len() != (slot.w as usize) * (slot.h as usize) {
            return;
        }
        self.pending.push((slot, coverage));
    }

    /// Write every queued bitmap into its page; whether anything was written.
    pub fn flush<B: GpuBackend>(&mut self, backend: &mut B) -> bool {
        let wrote = !self.pending.is_empty();
        for (slot, coverage) in self.pending.drain(..) {
            let texture = self.pages[usize::from(slot.page)].0;
            backend.write_texture(texture, slot.x, slot.y, slot.w, slot.h, &coverage);
        }
        wrote
    }
}

/// A new `edge × edge` page with no coverage, so linear sampling at a slot's
/// rim reads zeros rather than whatever the allocation held.
fn cleared_page<B: GpuBackend>(backend: &mut B, edge: u32) -> TextureId {
    let texture = backend.create_texture(&TextureDesc {
        width: edge,
        height: edge,
        format: MaskPages::FORMAT,
        render_target: false,
        label: "mask-page",
    });
    let band = (CLEAR_BAND / edge as usize).clamp(1, edge as usize) as u32;
    let zeros = vec![0u8; (band * edge) as usize];
    let mut y = 0;
    while y < edge {
        let rows = band.min(edge - y);
        backend.write_texture(texture, 0, y, edge, rows, &zeros[..(rows * edge) as usize]);
        y += rows;
    }
    texture
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mask::MaskFormat;
    use viso_gpu::CpuRaster;

    fn slot(page: u16, x: u32, y: u32, w: u32, h: u32) -> MaskSlot {
        MaskSlot {
            format: MaskFormat::R8,
            page,
            x,
            y,
            w,
            h,
        }
    }

    /// Pages follow the cache's list: created when it grows, given back when
    /// it shrinks, untouched while it holds still.
    #[test]
    fn pages_follow_the_cache() {
        let mut backend = CpuRaster::new();
        let mut pages = MaskPages::default();
        assert_eq!(pages.bytes(), 0);
        pages.sync(&mut backend, [16, 32].into_iter());
        assert_eq!(pages.bytes(), 16 * 16 + 32 * 32);
        let first = pages.texture(0);
        pages.sync(&mut backend, [16, 32].into_iter());
        assert_eq!(pages.texture(0), first);
        pages.sync(&mut backend, [16].into_iter());
        assert_eq!(pages.bytes(), 16 * 16);
        assert!(pages.contains(first));
    }

    /// A queued bitmap is written once, and only into a slot that fits.
    #[test]
    fn a_blit_is_written_once() {
        let mut backend = CpuRaster::new();
        let mut pages = MaskPages::default();
        pages.sync(&mut backend, [16].into_iter());
        pages.blit(slot(0, 3, 4, 2, 2), vec![7; 4]);
        pages.blit(slot(0, 15, 15, 2, 2), vec![7; 4]);
        pages.blit(slot(0, 0, 0, 2, 2), vec![1, 2, 3]);
        pages.blit(slot(1, 0, 0, 1, 1), vec![1]);
        assert_eq!(pages.pending.len(), 1);
        assert!(pages.flush(&mut backend));
        assert!(!pages.flush(&mut backend));
    }

    /// A page the cache releases drops what was queued for it.
    #[test]
    fn a_released_page_drops_its_blits() {
        let mut backend = CpuRaster::new();
        let mut pages = MaskPages::default();
        pages.sync(&mut backend, [16, 16].into_iter());
        pages.blit(slot(1, 0, 0, 1, 1), vec![9]);
        pages.sync(&mut backend, [16].into_iter());
        assert!(!pages.flush(&mut backend));
    }

    /// The repack signal round-trips through the pages.
    #[test]
    fn reblit_flag_round_trips() {
        let mut pages = MaskPages::default();
        assert!(!pages.needs_full_reblit());
        pages.set_needs_full_reblit(true);
        assert!(pages.needs_full_reblit());
    }
}
