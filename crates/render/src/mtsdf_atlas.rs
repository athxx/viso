//! The MTSDF field atlas: an RGBA8 *data* texture the renderer packs generated
//! multi-channel distance fields into, plus the CPU backing it uploads from.
//!
//! Structurally a sibling of [`ColorAtlas`](crate::color_atlas::ColorAtlas) —
//! the same paged plane, four bytes per texel, page-granular residency — but the
//! four channels are not a color. Three carry per-edge signed distances and the
//! fourth the true signed distance, so this texture is created as
//! [`TextureFormat::Rgba8Data`]: never premultiplied, never sRGB-encoded, never
//! promoted to a wider color domain. A distance is geometry, and a color domain
//! has nothing to say about it (§13.2).
//!
//! Its pages are budgeted by the MTSDF pool of the caller's residency,
//! independently of the coverage and color atlases.

use viso_gpu::{TextureFormat, TextureId};
use viso_text::mtsdf::{DISTANCE_RANGE, MtsdfGlyph};

use crate::atlas_plane::AtlasPlane;
use crate::primitive::Rect;

/// Bytes per texel of the RGBA8 backing.
const BPT: u32 = 4;

/// An RGBA8 MTSDF field atlas: a paged plane plus the GPU texture its texels
/// back.
///
/// The GPU [`TextureId`] is created once by the caller (the renderer owns device
/// resource creation) and handed in; this type never touches the device.
#[derive(Debug)]
pub struct MtsdfAtlas {
    /// The paged texel plane: per-page packers, CPU texels, dirty rect.
    plane: AtlasPlane,
    /// The GPU texture (`Rgba8Data`, `size × size`) these texels back.
    texture: TextureId,
}

/// The result of [`MtsdfAtlas::alloc_in_page`].
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum MtsdfAlloc {
    /// The field was packed at this normalized UV sub-rect.
    Placed(Rect),
    /// The glyph has no outline (a space); nothing packed.
    Empty,
    /// The named page has no room left. The caller reclaims a page,
    /// [`reset_page`](MtsdfAtlas::reset_page)s it, and retries there; nothing was
    /// modified by this call.
    PageFull,
    /// The field is larger than a whole page — no eviction can make it fit.
    TooLarge,
}

impl MtsdfAtlas {
    /// A fresh atlas of `size × size` texels cut into `page_size × page_size`
    /// pages, backing the given GPU texture.
    ///
    /// The texture must be created as [`TextureFormat::Rgba8Data`] at the same
    /// dimensions.
    pub fn new(size: u32, page_size: u32, texture: TextureId) -> Self {
        Self {
            plane: AtlasPlane::new(size, page_size, BPT),
            texture,
        }
    }

    /// The pixel format an MTSDF-atlas texture must be created with.
    pub const FORMAT: TextureFormat = TextureFormat::Rgba8Data;

    /// The distance range, in field texels, every field in this atlas encodes.
    ///
    /// One number for the whole atlas, not per glyph: the shader reads it as a
    /// constant, so a field generated with a different range could not share this
    /// texture. It is the generator's range
    /// ([`viso_text::mtsdf::DISTANCE_RANGE`]) re-exported here so the pipeline
    /// side has one place to read it from.
    pub const fn distance_range() -> f32 {
        DISTANCE_RANGE
    }

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

    /// The GPU texture handle these texels back.
    pub fn texture(&self) -> TextureId {
        self.texture
    }

    /// The full CPU texel backing (row-major RGBA8, `size² × 4` bytes).
    pub fn texels(&self) -> &[u8] {
        self.plane.pixels()
    }

    /// Pack `field` — the RGBA bytes [`MtsdfGenerator`] wrote for `glyph` — into
    /// `page`, returning where it landed.
    ///
    /// A glyph with no outline is [`MtsdfAlloc::Empty`] with no allocation.
    ///
    /// [`MtsdfGenerator`]: viso_text::mtsdf::MtsdfGenerator
    pub fn alloc_in_page(&mut self, page: usize, glyph: &MtsdfGlyph, field: &[u8]) -> MtsdfAlloc {
        let (gw, gh) = (glyph.width, glyph.height);
        if glyph.is_empty() || field.len() < (gw * gh * BPT) as usize {
            return MtsdfAlloc::Empty;
        }
        // 1-texel gutter so bilinear sampling never reaches a neighbor — or the
        // adjacent page — from this field's border. The gutter stays zero, which
        // in this encoding is the farthest *outside* distance, so a sample that
        // does graze it reads "well outside the glyph" rather than a neighbor's
        // edge. The field's own `FIELD_PAD` texels already keep the real edge
        // away from its border, so no legitimate sample lands here.
        let (pad_w, pad_h) = (gw + 2, gh + 2);
        if self.plane.exceeds_page(pad_w, pad_h) {
            return MtsdfAlloc::TooLarge;
        }
        let Some((px, py)) = self.plane.place(page, pad_w, pad_h) else {
            return MtsdfAlloc::PageFull;
        };
        let (gx, gy) = (px + 1, py + 1);
        self.plane.blit(gx, gy, gw, gh, field);
        MtsdfAlloc::Placed(self.plane.uv(gx, gy, gw, gh))
    }

    /// Reopen `page` for packing after residency reclaimed it. Only that page's
    /// packer is reset; no texels are cleared and no upload is triggered.
    pub fn reset_page(&mut self, page: usize) {
        self.plane.reset_page(page);
    }

    /// Take the accumulated dirty sub-rect (in texels) and its RGBA bytes,
    /// resetting the dirty state. Returns `None` when nothing changed.
    pub fn take_dirty(&mut self) -> Option<(u32, u32, u32, u32, Vec<u8>)> {
        self.plane.take_dirty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use viso_text::{FontFaceId, FontRevision};

    fn field(w: u32, h: u32) -> (MtsdfGlyph, Vec<u8>) {
        let meta = MtsdfGlyph {
            face: FontFaceId(0),
            glyph: 1,
            revision: FontRevision(0),
            generator_revision: 1,
            px_per_em: 32.0,
            width: w,
            height: h,
            left: 0.0,
            top: 0.0,
            distance_range: DISTANCE_RANGE,
            min_px_per_em: 22.0,
            max_px_per_em: 72.0,
        };
        let mut bytes = Vec::new();
        for i in 0..(w * h) {
            bytes.extend_from_slice(&[i as u8, 7, 9, 200]);
        }
        (meta, bytes)
    }

    #[test]
    fn an_outlineless_glyph_allocates_nothing() {
        let mut atlas = MtsdfAtlas::new(64, 64, TextureId::new(0));
        let (meta, bytes) = field(0, 0);
        assert_eq!(atlas.alloc_in_page(0, &meta, &bytes), MtsdfAlloc::Empty);
        assert!(atlas.take_dirty().is_none());
    }

    #[test]
    fn placed_uv_is_normalized_and_gutter_offset() {
        let mut atlas = MtsdfAtlas::new(64, 64, TextureId::new(0));
        let (meta, bytes) = field(8, 8);
        let MtsdfAlloc::Placed(uv) = atlas.alloc_in_page(0, &meta, &bytes) else {
            panic!("should place");
        };
        assert_eq!((uv.x, uv.y), (1.0 / 64.0, 1.0 / 64.0));
        assert_eq!((uv.w, uv.h), (8.0 / 64.0, 8.0 / 64.0));
    }

    #[test]
    fn the_field_uploads_verbatim() {
        // Four independent channels: nothing premultiplies, reorders, or
        // rescales them between the generator and the texture.
        let mut atlas = MtsdfAtlas::new(64, 64, TextureId::new(0));
        let (meta, bytes) = field(4, 4);
        atlas.alloc_in_page(0, &meta, &bytes);
        let (x, y, w, h, up) = atlas.take_dirty().expect("dirty after alloc");
        assert_eq!((x, y, w, h), (1, 1, 4, 4));
        assert_eq!(up, bytes);
        assert!(atlas.take_dirty().is_none());
    }

    #[test]
    fn a_full_page_reports_itself_and_destroys_nothing() {
        let mut atlas = MtsdfAtlas::new(24, 12, TextureId::new(0));
        let (meta, bytes) = field(8, 8);
        let MtsdfAlloc::Placed(first) = atlas.alloc_in_page(0, &meta, &bytes) else {
            panic!("first fits")
        };
        atlas.take_dirty();

        assert_eq!(
            atlas.alloc_in_page(0, &meta, &bytes),
            MtsdfAlloc::PageFull,
            "a full page reports itself"
        );
        assert!(atlas.take_dirty().is_none(), "and uploads nothing");

        atlas.reset_page(0);
        let MtsdfAlloc::Placed(again) = atlas.alloc_in_page(0, &meta, &bytes) else {
            panic!("page 0 reopened")
        };
        assert_eq!((first.x, first.y), (again.x, again.y));
    }

    #[test]
    fn a_field_larger_than_a_page_is_too_large() {
        let mut atlas = MtsdfAtlas::new(64, 8, TextureId::new(0));
        let (meta, bytes) = field(8, 8);
        assert_eq!(atlas.alloc_in_page(0, &meta, &bytes), MtsdfAlloc::TooLarge);
        assert!(atlas.take_dirty().is_none());
    }

    #[test]
    fn the_atlas_range_is_the_generator_range() {
        // The shader bakes this number in; a field generated against a different
        // range would decode at the wrong sharpness in this atlas.
        assert_eq!(MtsdfAtlas::distance_range(), DISTANCE_RANGE);
        assert_eq!(MtsdfAtlas::FORMAT, TextureFormat::Rgba8Data);
    }
}
