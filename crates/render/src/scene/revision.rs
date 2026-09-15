//! Separate monotone revision planes for the retained scene (§8.4).
//!
//! Each plane is an independent `u64` bump counter. The diff (F3.2) bumps only
//! the plane a change touches, so a paint-only edit — a recolor, an opacity
//! tween — advances `paint` and leaves `geometry` exactly where it was, which is
//! what lets the tessellation cache and the geometry stores stay untouched. A
//! consumer (a chunk, an upload plan, a bounds cache) records the plane values
//! it last observed and rebuilds only when the planes it depends on have moved.
//!
//! The planes are deliberately narrow and orthogonal (§11's dirty classes,
//! seen from the renderer): geometry vs paint vs transform vs clip vs resource
//! vs effect vs visibility. Collapsing them into one counter would make every
//! local change look like a full-scene change to every consumer — the exact
//! cost this separation exists to avoid.

/// The seven revision planes tracked by the scene. Cheap to copy (56 bytes of
/// plain counters); a consumer snapshots it and compares field-wise.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Revisions {
    /// Bumped when a shape changes: path commands, quad rect/radius, mesh
    /// vertices — anything that invalidates tessellation or geometry bounds.
    pub geometry: u64,
    /// Bumped when paint changes without geometry: fill/stroke color, tint,
    /// layer opacity. The plane a recolor touches, and only it.
    pub paint: u64,
    /// Bumped when a transform changes: a pure move/scale that shifts geometry
    /// without changing its shape (§8.5).
    pub transform: u64,
    /// Bumped when a clip rect or the clip stack structure changes.
    pub clip: u64,
    /// Bumped when a bound GPU resource changes: an image's or glyph run's
    /// texture identity.
    pub resource: u64,
    /// Bumped when a layer/filter effect changes (offscreen compositing).
    pub effect: u64,
    /// Bumped when visibility changes: a primitive entering or leaving the
    /// culled set. Populated by F4's visibility pass.
    pub visibility: u64,
}

impl Revisions {
    /// A fresh set, every plane at zero.
    pub const fn new() -> Revisions {
        Revisions {
            geometry: 0,
            paint: 0,
            transform: 0,
            clip: 0,
            resource: 0,
            effect: 0,
            visibility: 0,
        }
    }

    /// Advance the geometry plane. A shape changed.
    #[inline]
    pub fn bump_geometry(&mut self) {
        self.geometry = self.geometry.wrapping_add(1);
    }

    /// Advance the paint plane. A color/tint/opacity changed, geometry did not.
    #[inline]
    pub fn bump_paint(&mut self) {
        self.paint = self.paint.wrapping_add(1);
    }

    /// Advance the transform plane. A pure move/scale happened.
    #[inline]
    pub fn bump_transform(&mut self) {
        self.transform = self.transform.wrapping_add(1);
    }

    /// Advance the clip plane. A clip rect or the clip structure changed.
    #[inline]
    pub fn bump_clip(&mut self) {
        self.clip = self.clip.wrapping_add(1);
    }

    /// Advance the resource plane. A bound texture identity changed.
    #[inline]
    pub fn bump_resource(&mut self) {
        self.resource = self.resource.wrapping_add(1);
    }

    /// Advance the effect plane. A layer/filter effect changed.
    #[inline]
    pub fn bump_effect(&mut self) {
        self.effect = self.effect.wrapping_add(1);
    }

    /// Advance the visibility plane. The culled set changed.
    #[inline]
    pub fn bump_visibility(&mut self) {
        self.visibility = self.visibility.wrapping_add(1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn planes_are_independent() {
        let mut r = Revisions::new();
        let before = r;
        r.bump_paint();
        // Only paint moved; geometry (and every other plane) is untouched — the
        // core §8.4 guarantee a paint-only change must satisfy.
        assert_eq!(r.paint, before.paint + 1);
        assert_eq!(r.geometry, before.geometry);
        assert_eq!(r.transform, before.transform);
        assert_eq!(r.clip, before.clip);
        assert_eq!(r.resource, before.resource);
        assert_eq!(r.effect, before.effect);
        assert_eq!(r.visibility, before.visibility);
    }

    #[test]
    fn default_is_all_zero() {
        assert_eq!(Revisions::default(), Revisions::new());
        assert_eq!(Revisions::new().geometry, 0);
    }
}
