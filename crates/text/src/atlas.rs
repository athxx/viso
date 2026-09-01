//! Glyph atlas: an online MaxRects packer over a single R8 SDF texture.
//!
//! Scope for Phase 2: one fixed-size single-channel (R8) atlas. Glyphs are
//! rasterized to SDF bitmaps (see [`crate::raster`]) and packed with a MaxRects
//! best-short-side-fit heuristic. Successful packs are recorded so repeated
//! requests hit the cache. Writes accumulate into a single dirty rectangle so
//! the caller can upload one contiguous region per frame. When the atlas fills
//! up it is cleared and rebuilt from scratch (no grow, no per-glyph eviction) —
//! a size-growable / LRU-evicting atlas is deferred past Phase 2.
//!
//! The atlas owns only CPU pixels and packing state; it never touches the GPU.
//! The caller creates the R8 texture and uploads [`Atlas::dirty`] rows.

use crate::FontId;
use crate::font::FontFace;
use crate::raster::{RasterGlyph, rasterize_glyph};
use std::collections::HashMap;

/// Default square atlas edge, in texels.
pub const ATLAS_SIZE: u32 = 512;

/// A packed glyph's placement within the atlas.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct AtlasEntry {
    /// Texel x of the SDF bitmap's top-left corner.
    pub x: u32,
    /// Texel y of the SDF bitmap's top-left corner.
    pub y: u32,
    /// SDF bitmap width in texels.
    pub width: u32,
    /// SDF bitmap height in texels.
    pub height: u32,
    /// Offset from the pen origin to the bitmap's top-left, in pixels.
    pub bearing_px: [f32; 2],
    /// Coverage-ramp width for the shader (see [`crate::raster`]).
    pub px_range: f32,
}

impl AtlasEntry {
    /// UV sub-rectangle `[u_min, v_min, u_max, v_max]` for `atlas_size` texels.
    pub fn uv(&self, atlas_size: u32) -> [f32; 4] {
        let s = atlas_size as f32;
        [
            self.x as f32 / s,
            self.y as f32 / s,
            (self.x + self.width) as f32 / s,
            (self.y + self.height) as f32 / s,
        ]
    }
}

/// Cache key: a face, glyph id, and quantized raster density. Two requests at
/// nearly the same size share one raster (density is rounded to whole `dpx`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
struct GlyphKey {
    font: FontId,
    glyph_id: u16,
    /// `dpx_per_em` rounded to the nearest texel — the quantization bucket.
    dpx_q: u32,
}

/// A free rectangle in the MaxRects packer.
#[derive(Debug, Clone, Copy)]
struct FreeRect {
    x: u32,
    y: u32,
    w: u32,
    h: u32,
}

/// A dirty region covering all texels written since the last clear/upload.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct DirtyRect {
    pub x: u32,
    pub y: u32,
    pub w: u32,
    pub h: u32,
}

/// A single-channel SDF glyph atlas with an online MaxRects packer.
pub struct Atlas {
    size: u32,
    /// R8 pixels, row-major, `size * size` long.
    pixels: Vec<u8>,
    /// Maximal free rectangles not yet occupied.
    free: Vec<FreeRect>,
    /// Packed-glyph cache.
    entries: HashMap<GlyphKey, AtlasEntry>,
    /// Union of texels written since the last [`Atlas::take_dirty`].
    dirty: Option<DirtyRect>,
}

impl Atlas {
    /// A fresh empty atlas of `size` texels per side.
    pub fn new(size: u32) -> Self {
        Self {
            size,
            pixels: vec![0u8; (size * size) as usize],
            free: vec![FreeRect {
                x: 0,
                y: 0,
                w: size,
                h: size,
            }],
            entries: HashMap::new(),
            dirty: None,
        }
    }

    /// Atlas edge length in texels.
    pub fn size(&self) -> u32 {
        self.size
    }

    /// The full R8 pixel buffer.
    pub fn pixels(&self) -> &[u8] {
        &self.pixels
    }

    /// Take the accumulated dirty rectangle, resetting it to empty. `None` when
    /// nothing changed since the last call.
    pub fn take_dirty(&mut self) -> Option<DirtyRect> {
        self.dirty.take()
    }

    /// Get (rasterizing + packing on a miss) the atlas placement for a glyph.
    ///
    /// Returns `None` for glyphs with no outline (whitespace). If the glyph does
    /// not fit, the atlas is cleared and rebuilt once; a second failure (a glyph
    /// larger than the whole atlas) also returns `None`.
    pub fn glyph(
        &mut self,
        face: &FontFace,
        font: FontId,
        glyph_id: u16,
        dpx_per_em: f32,
    ) -> Option<AtlasEntry> {
        let key = GlyphKey {
            font,
            glyph_id,
            dpx_q: dpx_per_em.round() as u32,
        };
        if let Some(e) = self.entries.get(&key) {
            return Some(*e);
        }
        let raster = rasterize_glyph(face, glyph_id, dpx_per_em)?;
        if let Some(entry) = self.insert(&raster) {
            self.entries.insert(key, entry);
            return Some(entry);
        }
        // Full: clear and retry once from a clean slate.
        self.reset();
        let entry = self.insert(&raster)?;
        self.entries.insert(key, entry);
        Some(entry)
    }

    /// Clear all pixels, packing state, and cache; mark the whole atlas dirty.
    fn reset(&mut self) {
        self.pixels.iter_mut().for_each(|p| *p = 0);
        self.free = vec![FreeRect {
            x: 0,
            y: 0,
            w: self.size,
            h: self.size,
        }];
        self.entries.clear();
        self.dirty = Some(DirtyRect {
            x: 0,
            y: 0,
            w: self.size,
            h: self.size,
        });
    }

    /// Pack a rasterized glyph, copy its pixels in, and record the placement.
    /// Returns `None` if it does not fit the current free space.
    fn insert(&mut self, raster: &RasterGlyph) -> Option<AtlasEntry> {
        let (x, y) = self.pack(raster.width, raster.height)?;
        self.blit(x, y, raster);
        Some(AtlasEntry {
            x,
            y,
            width: raster.width,
            height: raster.height,
            bearing_px: raster.bearing_px,
            px_range: raster.px_range,
        })
    }

    /// MaxRects best-short-side-fit: pick the free rect whose leftover short
    /// side is smallest (ties broken by long side, then area), place at its
    /// top-left, then split every overlapping free rect and prune contained
    /// ones. Returns the chosen top-left, or `None` if nothing fits.
    fn pack(&mut self, w: u32, h: u32) -> Option<(u32, u32)> {
        let mut best: Option<(usize, u32, u32, u32)> = None; // (idx, short, long, area)
        for (i, r) in self.free.iter().enumerate() {
            if r.w < w || r.h < h {
                continue;
            }
            let leftover_w = r.w - w;
            let leftover_h = r.h - h;
            let short = leftover_w.min(leftover_h);
            let long = leftover_w.max(leftover_h);
            let area = r.w * r.h;
            let better = match best {
                None => true,
                Some((_, bs, bl, ba)) => (short, long, area) < (bs, bl, ba),
            };
            if better {
                best = Some((i, short, long, area));
            }
        }
        let (idx, ..) = best?;
        let placed = self.free[idx];
        let (px, py) = (placed.x, placed.y);
        let used = FreeRect { x: px, y: py, w, h };

        // Split every free rect that overlaps the placed region.
        let mut next = Vec::with_capacity(self.free.len() + 4);
        for r in self.free.drain(..) {
            if let Some(pieces) = split_free(r, used) {
                next.extend(pieces);
            } else {
                next.push(r);
            }
        }
        // Prune rects fully contained in another.
        let mut pruned: Vec<FreeRect> = Vec::with_capacity(next.len());
        'outer: for (i, a) in next.iter().enumerate() {
            for (j, b) in next.iter().enumerate() {
                if i != j && contains(*b, *a) && !(i > j && contains(*a, *b)) {
                    continue 'outer;
                }
            }
            pruned.push(*a);
        }
        self.free = pruned;
        Some((px, py))
    }

    /// Copy a glyph's SDF rows into the atlas at `(x, y)` and grow the dirty rect.
    fn blit(&mut self, x: u32, y: u32, raster: &RasterGlyph) {
        for row in 0..raster.height {
            let src = (row * raster.width) as usize;
            let dst = ((y + row) * self.size + x) as usize;
            let n = raster.width as usize;
            self.pixels[dst..dst + n].copy_from_slice(&raster.sdf[src..src + n]);
        }
        self.grow_dirty(DirtyRect {
            x,
            y,
            w: raster.width,
            h: raster.height,
        });
    }

    /// Union `r` into the accumulated dirty rectangle.
    fn grow_dirty(&mut self, r: DirtyRect) {
        self.dirty = Some(match self.dirty {
            None => r,
            Some(d) => {
                let x0 = d.x.min(r.x);
                let y0 = d.y.min(r.y);
                let x1 = (d.x + d.w).max(r.x + r.w);
                let y1 = (d.y + d.h).max(r.y + r.h);
                DirtyRect {
                    x: x0,
                    y: y0,
                    w: x1 - x0,
                    h: y1 - y0,
                }
            }
        });
    }
}

/// Whether `outer` fully contains `inner`.
fn contains(outer: FreeRect, inner: FreeRect) -> bool {
    inner.x >= outer.x
        && inner.y >= outer.y
        && inner.x + inner.w <= outer.x + outer.w
        && inner.y + inner.h <= outer.y + outer.h
}

/// Split `r` around the placed rectangle `used`. Returns `None` if they do not
/// overlap (caller keeps `r` unchanged); otherwise the up-to-four remaining
/// maximal strips of `r` outside `used`.
fn split_free(r: FreeRect, used: FreeRect) -> Option<Vec<FreeRect>> {
    let (rx0, ry0, rx1, ry1) = (r.x, r.y, r.x + r.w, r.y + r.h);
    let (ux0, uy0, ux1, uy1) = (used.x, used.y, used.x + used.w, used.y + used.h);
    if ux0 >= rx1 || ux1 <= rx0 || uy0 >= ry1 || uy1 <= ry0 {
        return None; // disjoint
    }
    let mut pieces = Vec::with_capacity(4);
    // Left strip.
    if ux0 > rx0 {
        pieces.push(FreeRect {
            x: rx0,
            y: ry0,
            w: ux0 - rx0,
            h: r.h,
        });
    }
    // Right strip.
    if ux1 < rx1 {
        pieces.push(FreeRect {
            x: ux1,
            y: ry0,
            w: rx1 - ux1,
            h: r.h,
        });
    }
    // Top strip.
    if uy0 > ry0 {
        pieces.push(FreeRect {
            x: rx0,
            y: ry0,
            w: r.w,
            h: uy0 - ry0,
        });
    }
    // Bottom strip.
    if uy1 < ry1 {
        pieces.push(FreeRect {
            x: rx0,
            y: uy1,
            w: r.w,
            h: ry1 - uy1,
        });
    }
    Some(pieces)
}
