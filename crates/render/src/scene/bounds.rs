//! Bounds a retained primitive carries (§8).
//!
//! Each primitive resolves through the same pipeline of rectangles, computed
//! without ever re-parsing a path — the geometry store hands over the tight
//! extent, and inflation for stroke and filter is applied numerically:
//!
//! - `world` — the primitive's own extent in world space, tight to the
//!   geometry (fill only). A retained primitive carries no transform of its
//!   own, so this is also its local extent.
//! - `paint` — `geometry + stroke inflation + filter inflation`: the region a
//!   consumer must actually redraw and re-upload when this primitive is dirty.
//!   Half the stroke width bleeds outside the fill; a filter (blur) inflates
//!   further. This is the bound damage, culling and visibility use; no effect
//!   reads beyond it.
//!
//! The clipped extent is `world` cut by the clip the entry was emitted under,
//! derived where it is read rather than stored on every entry.

use crate::primitive::Rect;

/// The bounds of one retained primitive (§8). Plain rectangles, cheap to copy.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Bounds {
    /// Tight fill extent in world space.
    pub world: Rect,
    /// Redraw/upload region: geometry + stroke inflation + filter inflation.
    pub paint: Rect,
}

impl Bounds {
    /// Build the bounds for a primitive whose tight world-space extent is
    /// `world`, with `stroke` width centered on the outline and `filter`
    /// inflation (both in physical pixels, `0` when absent).
    ///
    /// Half the stroke lies outside the fill, so the paint region grows by that
    /// half-width on every side. The computation is pure arithmetic on the
    /// already-resolved extent — no path re-parse.
    pub fn from_world(world: Rect, stroke: f32, filter: f32) -> Bounds {
        Bounds {
            world,
            paint: world.inflate(stroke * 0.5 + filter),
        }
    }

    /// `world` cut by `clip`, the clip the primitive was emitted under.
    pub fn clipped(&self, clip: Option<Rect>) -> Rect {
        match clip {
            Some(c) => self.world.intersect(c),
            None => self.world,
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
        let b = Bounds::from_world(world, 4.0, 0.0);
        // Half of the 4px stroke (2px) bleeds outside the fill on every side.
        assert_eq!(b.paint, rect(8.0, 8.0, 24.0, 24.0));
        assert_eq!(b.world, world);
    }

    #[test]
    fn fill_only_paint_equals_geometry() {
        let world = rect(0.0, 0.0, 16.0, 8.0);
        let b = Bounds::from_world(world, 0.0, 0.0);
        assert_eq!(b.paint, world);
    }

    #[test]
    fn clip_narrows_the_clipped_extent_only() {
        let world = rect(0.0, 0.0, 100.0, 100.0);
        let b = Bounds::from_world(world, 0.0, 0.0);
        let clip = Some(rect(10.0, 10.0, 20.0, 20.0));
        assert_eq!(b.clipped(clip), rect(10.0, 10.0, 20.0, 20.0));
        assert_eq!(b.paint, world);
    }
}
