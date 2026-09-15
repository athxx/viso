//! Bounds a retained primitive carries, at five stages (§8).
//!
//! Each primitive resolves through the same pipeline of rectangles, computed
//! without ever re-parsing a path — the geometry store hands over the tight
//! local extent, and inflation for stroke and filter is applied numerically:
//!
//! - `local`  — the primitive's own extent in its own coordinate space, tight
//!   to the geometry (fill only).
//! - `world`  — `local` after the primitive's transform, in world space.
//! - `clip`   — `world` intersected with the effective clip stack.
//! - `paint`  — `geometry + stroke inflation + filter inflation`: the region a
//!   consumer must actually redraw and re-upload when this primitive is dirty.
//!   Half the stroke width bleeds outside the fill; a filter (blur) inflates
//!   further. This is the bound F4's dirty coalescer and visibility use.
//! - `effect` — `paint` grown by any layer/effect that reads neighbouring
//!   pixels (offscreen compositing footprint).
//!
//! Storing all five, rather than recomputing on demand, is what keeps the hot
//! path free of path re-parsing: a transform-only change recomputes `world`
//! onward from the cached `local`; a paint-only change recomputes nothing.

use crate::primitive::Rect;

/// The five-stage bounds of one retained primitive (§8). Plain rectangles,
/// cheap to copy; recomputed field-wise as the planes they depend on move.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Bounds {
    /// Tight fill extent in the primitive's own space.
    pub local: Rect,
    /// `local` after the primitive's transform, in world space.
    pub world: Rect,
    /// `world` intersected with the effective clip.
    pub clip: Rect,
    /// Redraw/upload region: geometry + stroke inflation + filter inflation.
    pub paint: Rect,
    /// `paint` grown by neighbourhood-reading effects.
    pub effect: Rect,
}

impl Default for Bounds {
    fn default() -> Bounds {
        Bounds {
            local: Rect::ZERO,
            world: Rect::ZERO,
            clip: Rect::ZERO,
            paint: Rect::ZERO,
            effect: Rect::ZERO,
        }
    }
}

impl Bounds {
    /// Build the bounds for a primitive whose tight world-space extent is
    /// `world`, with no transform (identity), no clip narrower than the extent,
    /// `stroke` half-width bleeding outside the fill, and `filter` inflation
    /// (both in physical pixels, `0` when absent).
    ///
    /// Stroke is centered on the outline, so half the width lies outside the
    /// fill; the paint region grows by that half-width on every side. The
    /// computation is pure arithmetic on the already-resolved extent — no path
    /// is re-parsed (§8).
    pub fn from_world(world: Rect, clip: Option<Rect>, stroke: f32, filter: f32) -> Bounds {
        let clip_rect = match clip {
            Some(c) => world.intersect(c),
            None => world,
        };
        let paint = world.inflate(stroke * 0.5 + filter);
        Bounds {
            local: world,
            world,
            clip: clip_rect,
            paint,
            effect: paint,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rect(x: f32, y: f32, w: f32, h: f32) -> Rect {
        Rect { x, y, w, h }
    }

    #[test]
    fn stroke_inflates_paint_by_half_width() {
        let world = rect(10.0, 10.0, 20.0, 20.0);
        let b = Bounds::from_world(world, None, 4.0, 0.0);
        // Half of the 4px stroke (2px) bleeds outside the fill on every side.
        assert_eq!(b.paint, rect(8.0, 8.0, 24.0, 24.0));
        assert_eq!(b.local, world);
        assert_eq!(b.world, world);
    }

    #[test]
    fn fill_only_paint_equals_geometry() {
        let world = rect(0.0, 0.0, 16.0, 8.0);
        let b = Bounds::from_world(world, None, 0.0, 0.0);
        assert_eq!(b.paint, world);
        assert_eq!(b.effect, world);
    }

    #[test]
    fn clip_narrows_the_clip_bound_only() {
        let world = rect(0.0, 0.0, 100.0, 100.0);
        let b = Bounds::from_world(world, Some(rect(10.0, 10.0, 20.0, 20.0)), 0.0, 0.0);
        assert_eq!(b.clip, rect(10.0, 10.0, 20.0, 20.0));
        assert_eq!(b.world, world);
    }
}
