//! Glyph atlas: an online MaxRects packer over a single fixed-format texture.
//!
//! An atlas is built for one texel format — either single-channel R8 (SDF
//! outline glyphs, see [`crate::raster`]) or 4-channel RGBA8 (premultiplied
//! color-bitmap emoji, see [`crate::color_raster`]). Glyphs are packed with a
//! MaxRects best-short-side-fit heuristic; successful packs are recorded so
//! repeated requests hit the cache. Writes accumulate into a single dirty
//! rectangle so the caller can upload one contiguous region per frame. When the
//! atlas fills up it is cleared and rebuilt from scratch (no grow, no per-glyph
//! eviction) — a size-growable / LRU-evicting atlas is deferred.
//!
//! The packer itself is format-agnostic: only the pixel buffer size and the
//! per-row byte copy depend on the atlas's bytes-per-texel, so the SDF and color
//! atlases share one implementation and differ only in `bpp`.
//!
//! The atlas owns only CPU pixels and packing state; it never touches the GPU.
//! The caller creates the texture and uploads [`Atlas::take_dirty`] rows.

use crate::FontId;
use crate::color_raster::{ColorGlyph, ColorGlyphRasterizer, rasterize_color_glyph};
use crate::font::FontFace;
use crate::raster::rasterize_glyph;
use std::collections::HashMap;

/// Default square atlas edge, in texels.
pub const ATLAS_SIZE: u32 = 512;

/// Which raster kind an atlas / cache entry holds. SDF and color glyphs live in
/// separate atlases and never share a texel format, so the kind is part of the
/// cache key to keep the two glyph populations distinct.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum GlyphKind {
    /// Single-channel R8 SDF outline glyph.
    Sdf,
    /// 4-channel premultiplied-RGBA color-bitmap glyph.
    Color,
}

impl GlyphKind {
    /// Bytes per texel for this kind's atlas format.
    const fn bpp(self) -> u32 {
        match self {
            GlyphKind::Sdf => 1,
            GlyphKind::Color => 4,
        }
    }
}

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

/// Cache key: a face, glyph id, raster kind, and quantized raster density. Two
/// requests at nearly the same size share one raster (density is rounded to
/// whole `dpx`) — this rounding is the color size-bucket the plan calls for, so
/// many display sizes reuse one decoded emoji strike.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
struct GlyphKey {
    font: FontId,
    glyph_id: u16,
    kind: GlyphKind,
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

/// A fixed-format glyph atlas with an online MaxRects packer. The texel format
/// (R8 SDF or RGBA8 color) is fixed at construction; see [`GlyphKind`].
pub struct Atlas {
    size: u32,
    /// Which raster kind this atlas holds (fixes the texel format / `bpp`).
    kind: GlyphKind,
    /// Pixels, row-major, `size * size * kind.bpp()` long.
    pixels: Vec<u8>,
    /// Maximal free rectangles not yet occupied.
    free: Vec<FreeRect>,
    /// Packed-glyph cache.
    entries: HashMap<GlyphKey, AtlasEntry>,
    /// Union of texels written since the last [`Atlas::take_dirty`].
    dirty: Option<DirtyRect>,
}

impl Atlas {
    /// A fresh empty R8 SDF atlas of `size` texels per side.
    pub fn new(size: u32) -> Self {
        Self::with_kind(size, GlyphKind::Sdf)
    }

    /// A fresh empty RGBA8 color-bitmap atlas of `size` texels per side.
    pub fn new_color(size: u32) -> Self {
        Self::with_kind(size, GlyphKind::Color)
    }

    fn with_kind(size: u32, kind: GlyphKind) -> Self {
        Self {
            size,
            kind,
            pixels: vec![0u8; (size * size * kind.bpp()) as usize],
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

    /// The full pixel buffer (`size² * bpp` bytes; `bpp` is 1 for SDF, 4 for color).
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
    ///
    /// The `bool` is glyph freshness: `true` when this call rasterized and packed
    /// the glyph (an atlas miss), `false` on a dedup hit against an already-packed
    /// entry. The caller counts fresh rasters without a second lookup.
    pub fn glyph(
        &mut self,
        face: &FontFace,
        font: FontId,
        glyph_id: u16,
        dpx_per_em: f32,
    ) -> Option<(AtlasEntry, bool)> {
        debug_assert_eq!(self.kind, GlyphKind::Sdf);
        let key = GlyphKey {
            font,
            glyph_id,
            kind: GlyphKind::Sdf,
            dpx_q: dpx_per_em.round() as u32,
        };
        if let Some(e) = self.entries.get(&key) {
            return Some((*e, false));
        }
        let raster = rasterize_glyph(face, glyph_id, dpx_per_em)?;
        let entry = self.insert_cached(key, raster.width, raster.height, &raster.sdf, |r| {
            AtlasEntry {
                bearing_px: raster.bearing_px,
                px_range: raster.px_range,
                ..*r
            }
        })?;
        Some((entry, true))
    }

    /// Get (rasterizing + packing on a miss) the color-atlas placement for a
    /// glyph. Returns `None` for glyphs with no color-bitmap strike (the caller
    /// then treats the glyph as an SDF outline). For a color entry `bearing_px`
    /// is the strike origin offset scaled to `dpx_per_em`, and `px_range` is
    /// unused (the color path samples RGBA directly, no coverage ramp).
    ///
    /// A face marked [`FontFace::is_color_emoji`] has its strikes stripped and
    /// cannot be decoded by `ttf-parser`, so its color glyphs come from
    /// `color_raster` (the platform text engine, keyed on the face's PostScript
    /// name). Any other face keeps the in-crate `ttf-parser` strike path. When
    /// `color_raster` is `None` (no platform binding, e.g. wasm) an emoji face
    /// simply yields no color glyph and falls through to the outline path.
    ///
    /// The `bool` is glyph freshness: `true` when this call rasterized and packed
    /// the glyph (an atlas miss), `false` on a dedup hit — the same contract as
    /// [`Self::glyph`].
    pub fn color_glyph(
        &mut self,
        face: &FontFace,
        font: FontId,
        glyph_id: u16,
        dpx_per_em: f32,
        color_raster: Option<&dyn ColorGlyphRasterizer>,
    ) -> Option<(AtlasEntry, bool)> {
        debug_assert_eq!(self.kind, GlyphKind::Color);
        let key = GlyphKey {
            font,
            glyph_id,
            kind: GlyphKind::Color,
            dpx_q: dpx_per_em.round() as u32,
        };
        if let Some(e) = self.entries.get(&key) {
            return Some((*e, false));
        }
        let color: ColorGlyph = if face.is_color_emoji() {
            let ps_name = face.postscript_name()?;
            color_raster?.rasterize(&ps_name, face.glyph_count, glyph_id, dpx_per_em)?
        } else {
            rasterize_color_glyph(face, glyph_id, dpx_per_em)?
        };
        // Scale the strike's origin offset from its own ppem to the request.
        let scale = dpx_per_em / color.pixels_per_em as f32;
        let bearing = [color.origin_px[0] * scale, color.origin_px[1] * scale];
        let entry = self.insert_cached(key, color.width, color.height, &color.rgba, |r| {
            AtlasEntry {
                bearing_px: bearing,
                px_range: 0.0,
                ..*r
            }
        })?;
        Some((entry, true))
    }

    /// Pack `pixels` (a `w * h` bitmap of `kind.bpp()` bytes per texel), then
    /// build and cache its [`AtlasEntry`] via `make`, retrying once against a
    /// cleared atlas if the first pack fails. `make` receives a placed entry
    /// carrying the chosen `(x, y)` and fills in the glyph-specific fields.
    fn insert_cached(
        &mut self,
        key: GlyphKey,
        w: u32,
        h: u32,
        pixels: &[u8],
        make: impl Fn(&AtlasEntry) -> AtlasEntry,
    ) -> Option<AtlasEntry> {
        let entry = match self.insert(w, h, pixels, &make) {
            Some(e) => e,
            None => {
                // Full: clear and retry once from a clean slate.
                self.reset();
                self.insert(w, h, pixels, &make)?
            }
        };
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

    /// Pack a `w * h` bitmap, copy its pixels in, and build its entry via `make`
    /// (given a placeholder carrying the chosen `(x, y)`). Returns `None` if it
    /// does not fit the current free space.
    fn insert(
        &mut self,
        w: u32,
        h: u32,
        pixels: &[u8],
        make: impl Fn(&AtlasEntry) -> AtlasEntry,
    ) -> Option<AtlasEntry> {
        let (x, y) = self.pack(w, h)?;
        self.blit(x, y, w, h, pixels);
        Some(make(&AtlasEntry {
            x,
            y,
            width: w,
            height: h,
            bearing_px: [0.0, 0.0],
            px_range: 0.0,
        }))
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

    /// Copy a `w * h` bitmap's rows into the atlas at texel `(x, y)` and grow the
    /// dirty rect. Byte offsets scale by the atlas's bytes-per-texel.
    fn blit(&mut self, x: u32, y: u32, w: u32, h: u32, pixels: &[u8]) {
        let bpp = self.kind.bpp();
        let row_bytes = (w * bpp) as usize;
        for row in 0..h {
            let src = (row * w * bpp) as usize;
            let dst = ((y + row) * self.size * bpp + x * bpp) as usize;
            self.pixels[dst..dst + row_bytes].copy_from_slice(&pixels[src..src + row_bytes]);
        }
        self.grow_dirty(DirtyRect { x, y, w, h });
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn color_atlas_allocates_four_bytes_per_texel() {
        let a = Atlas::new_color(8);
        assert_eq!(a.pixels().len(), 8 * 8 * 4);
        // SDF atlas stays one byte per texel.
        assert_eq!(Atlas::new(8).pixels().len(), 8 * 8);
    }

    #[test]
    fn color_blit_places_rgba_rows_at_texel_offset() {
        let mut a = Atlas::new_color(4);
        // A 2x2 RGBA bitmap: four distinct opaque texels.
        let bmp: Vec<u8> = vec![
            1, 2, 3, 255, 4, 5, 6, 255, // row 0
            7, 8, 9, 255, 10, 11, 12, 255, // row 1
        ];
        let entry = a
            .insert(2, 2, &bmp, |r| AtlasEntry {
                bearing_px: [1.0, -2.0],
                px_range: 0.0,
                ..*r
            })
            .unwrap();
        assert_eq!((entry.width, entry.height), (2, 2));
        assert_eq!(entry.bearing_px, [1.0, -2.0]);
        // At top-left the first texel's four bytes land at buffer start.
        let (x, y) = (entry.x, entry.y);
        let bpp = 4u32;
        let base = ((y * 4 + x) * bpp) as usize;
        assert_eq!(&a.pixels()[base..base + 4], &[1, 2, 3, 255]);
        // Second row is a full stride (4 texels * 4 bytes) down.
        let row1 = (((y + 1) * 4 + x) * bpp) as usize;
        assert_eq!(&a.pixels()[row1..row1 + 4], &[7, 8, 9, 255]);
        // The whole 2x2 region is dirty.
        let d = a.take_dirty().unwrap();
        assert_eq!((d.w, d.h), (2, 2));
    }
}
