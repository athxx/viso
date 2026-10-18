//! Transform roots in the primitive stream (§21).
//!
//! A [`Primitive::Transform`](crate::Primitive::Transform) bracket names the
//! transform root its content moves with. Geometry inside it is written in world
//! space as of the frame it was painted, with the root's displacement at that
//! moment recorded as *baked*. A later frame that changes only displacements —
//! a scroll step, a slide — hands the new table to
//! [`Renderer::set_transforms`](crate::Renderer::set_transforms): each draw is
//! then moved by its root's `current - baked` through the `shift` uniform every
//! built-in reads, and each clip opened inside a root moves with it. No instance
//! is rewritten, no buffer is uploaded, and the cost is per draw, not per
//! primitive.
//!
//! A clip is recorded as a chain of links. A link is one layer clip pushed while
//! some root was open, together with that root, so its rect moves by that root's
//! shift; the chain's first link also carries the static clip it was opened
//! under. Links are interned per frame, so siblings that open equal clips share
//! one chain and their draws still batch.
//!
//! Content whose pixels are not a pure function of its own instances moved by a
//! shift — an offscreen layer (its region of interest was cut against the
//! surface and its clip), a backdrop or material (they sample what is behind),
//! a user program (it writes its own uniforms) — taints the innermost open
//! root. Moving a tainted root, or moving anything while the frame captures a
//! backdrop, reports [`TransformUpdate::NeedsUpload`]: the caller repaints at
//! the new displacements and uploads, which is always correct.

use std::collections::HashMap;

use crate::primitive::Rect;

/// No transform root: static content.
pub(crate) const NO_SLOT: u32 = u32::MAX;
/// No moving clip.
pub(crate) const NO_CHAIN: u32 = u32::MAX;

/// What an emit moves with: its innermost open transform root, and the chain of
/// moving clips constraining it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Space {
    /// The transform root, or `NO_SLOT`.
    pub slot: u32,
    /// The moving clip chain, or `NO_CHAIN`.
    pub chain: u32,
}

impl Space {
    /// Static content: no root, no moving clip.
    pub const STATIC: Space = Space {
        slot: NO_SLOT,
        chain: NO_CHAIN,
    };
}

/// What [`Renderer::set_transforms`](crate::Renderer::set_transforms) did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TransformUpdate {
    /// No root the uploaded frame painted moved.
    Unchanged,
    /// Roots moved; the next encode shifts their draws.
    Moved,
    /// A moved root holds content a shift cannot move: repaint at the new
    /// displacements and upload.
    NeedsUpload,
}

/// One clip pushed inside a transform root.
#[derive(Debug, Clone, Copy)]
struct ClipLink {
    /// The enclosing link, or `NO_CHAIN` for the first.
    parent: u32,
    /// The root the clip moves with (`NO_SLOT` when it was pushed outside one).
    slot: u32,
    /// The layer's clip rect, world space as painted.
    rect: Rect,
    /// For the first link: the static clip it was opened under.
    base: Option<Rect>,
}

/// The interning key of a link: its parent, root, rect and base, by bits.
type LinkKey = (u32, u32, [u32; 4], Option<[u32; 4]>);

fn rect_bits(r: Rect) -> [u32; 4] {
    [r.x.to_bits(), r.y.to_bits(), r.w.to_bits(), r.h.to_bits()]
}

/// The renderer's transform-root state: the open roots while a stream is
/// ingested, the clip links it recorded, and the displacement tables.
#[derive(Debug, Default)]
pub(crate) struct Transforms {
    /// The roots open at this point of the stream, innermost last.
    open: Vec<u32>,
    links: Vec<ClipLink>,
    interned: HashMap<LinkKey, u32>,
    /// Each root's displacement when the uploaded primitives were painted.
    baked: Vec<[f32; 2]>,
    /// Each root's displacement now.
    current: Vec<[f32; 2]>,
    /// Whether a shift moves each root's content correctly.
    plain: Vec<bool>,
    /// The frame captures a backdrop, which samples whatever moved under it.
    pinned: bool,
    /// Some root's current displacement differs from its baked one.
    moved: bool,
}

impl Transforms {
    /// Start ingesting a stream painted at `painted`.
    pub(crate) fn begin_frame(&mut self, painted: &[[f32; 2]]) {
        self.open.clear();
        self.links.clear();
        self.interned.clear();
        self.baked.clear();
        self.baked.extend_from_slice(painted);
        self.current.clear();
        self.current.extend_from_slice(painted);
        self.plain.clear();
        self.plain.resize(painted.len(), true);
        self.pinned = false;
        self.moved = false;
    }

    /// Record whether the ingested frame captures a backdrop.
    pub(crate) fn finish_frame(&mut self, captures: bool) {
        self.pinned = captures;
        self.open.clear();
    }

    /// The innermost open root.
    pub(crate) fn slot(&self) -> u32 {
        self.open.last().copied().unwrap_or(NO_SLOT)
    }

    /// Open root `slot`. Content routed to an offscreen pass is drawn in that
    /// pass's own space, which a shift does not reach, so a root opened there is
    /// tainted.
    pub(crate) fn push(&mut self, slot: u32, offscreen: bool) {
        self.open.push(slot);
        if offscreen {
            self.taint();
        }
    }

    /// Close the innermost root.
    pub(crate) fn pop(&mut self) {
        self.open.pop();
    }

    /// Mark the innermost open root as holding content a shift cannot move.
    pub(crate) fn taint(&mut self) {
        let slot = self.slot();
        if slot == NO_SLOT {
            return;
        }
        let i = slot as usize;
        if self.plain.len() <= i {
            self.plain.resize(i + 1, true);
        }
        self.plain[i] = false;
    }

    /// The chain for content under a layer clipping to `rect`, opened inside
    /// `parent` (whose effective clip, when `parent` is `NO_CHAIN`, is the
    /// static `base`). Outside every root a clip never moves: `NO_CHAIN`.
    pub(crate) fn open_clip(&mut self, parent: u32, base: Option<Rect>, rect: Rect) -> u32 {
        let slot = self.slot();
        if slot == NO_SLOT && parent == NO_CHAIN {
            return NO_CHAIN;
        }
        let base = if parent == NO_CHAIN { base } else { None };
        let key = (parent, slot, rect_bits(rect), base.map(rect_bits));
        if let Some(&chain) = self.interned.get(&key) {
            return chain;
        }
        let chain = self.links.len() as u32;
        self.links.push(ClipLink {
            parent,
            slot,
            rect,
            base,
        });
        self.interned.insert(key, chain);
        chain
    }

    /// The space an emit at this point of the stream belongs to.
    pub(crate) fn space(&self, chain: u32) -> Space {
        Space {
            slot: self.slot(),
            chain,
        }
    }

    /// Take the roots' displacements now.
    pub(crate) fn set(&mut self, current: &[[f32; 2]]) -> TransformUpdate {
        self.current.clear();
        self.current.extend_from_slice(&self.baked);
        let mut moved = false;
        for (slot, now) in current.iter().take(self.baked.len()).enumerate() {
            if *now == self.baked[slot] {
                continue;
            }
            if self.pinned || !self.plain.get(slot).copied().unwrap_or(true) {
                self.current.copy_from_slice(&self.baked);
                self.moved = false;
                return TransformUpdate::NeedsUpload;
            }
            self.current[slot] = *now;
            moved = true;
        }
        self.moved = moved;
        if moved {
            TransformUpdate::Moved
        } else {
            TransformUpdate::Unchanged
        }
    }

    /// How far root `slot` moved since its content was painted.
    pub(crate) fn shift(&self, slot: u32) -> [f32; 2] {
        if !self.moved || slot == NO_SLOT {
            return [0.0, 0.0];
        }
        let i = slot as usize;
        match (self.current.get(i), self.baked.get(i)) {
            (Some(now), Some(then)) => [now[0] - then[0], now[1] - then[1]],
            _ => [0.0, 0.0],
        }
    }

    /// The clip of an emit in `space` now, given the clip it was painted with.
    pub(crate) fn clip(&self, space: Space, painted: Option<Rect>) -> Option<Rect> {
        if !self.moved || space.chain == NO_CHAIN {
            return painted;
        }
        let mut at = space.chain;
        let mut clip = Rect::INFINITE;
        while let Some(link) = self.links.get(at as usize) {
            let [dx, dy] = self.shift(link.slot);
            clip = clip.intersect(Rect {
                x: link.rect.x + dx,
                y: link.rect.y + dy,
                w: link.rect.w,
                h: link.rect.h,
            });
            if link.parent == NO_CHAIN {
                if let Some(base) = link.base {
                    clip = clip.intersect(base);
                }
                break;
            }
            at = link.parent;
        }
        Some(clip)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rect(x: f32, y: f32, w: f32, h: f32) -> Rect {
        Rect { x, y, w, h }
    }

    #[test]
    fn a_clip_outside_every_root_never_moves() {
        let mut t = Transforms::default();
        t.begin_frame(&[]);
        assert_eq!(
            t.open_clip(NO_CHAIN, None, rect(0.0, 0.0, 10.0, 10.0)),
            NO_CHAIN
        );
    }

    #[test]
    fn a_clip_inside_a_root_moves_with_it_under_its_static_base() {
        let mut t = Transforms::default();
        t.begin_frame(&[[0.0, 0.0]]);
        let viewport = Some(rect(0.0, 0.0, 100.0, 100.0));
        t.push(0, false);
        let chain = t.open_clip(NO_CHAIN, viewport, rect(0.0, 50.0, 100.0, 80.0));
        let space = t.space(chain);
        t.pop();
        t.finish_frame(false);
        assert_eq!(t.set(&[[0.0, -30.0]]), TransformUpdate::Moved);
        assert_eq!(t.shift(0), [0.0, -30.0]);
        assert_eq!(
            t.clip(space, Some(rect(0.0, 50.0, 100.0, 50.0))),
            Some(rect(0.0, 20.0, 100.0, 80.0))
        );
    }

    #[test]
    fn equal_sibling_clips_share_one_chain() {
        let mut t = Transforms::default();
        t.begin_frame(&[[0.0, 0.0]]);
        t.push(0, false);
        let a = t.open_clip(NO_CHAIN, None, rect(0.0, 0.0, 5.0, 5.0));
        let b = t.open_clip(NO_CHAIN, None, rect(0.0, 0.0, 5.0, 5.0));
        assert_eq!(a, b);
    }

    #[test]
    fn moving_a_tainted_root_or_under_a_capture_needs_an_upload() {
        let mut t = Transforms::default();
        t.begin_frame(&[[0.0, 0.0], [0.0, 0.0]]);
        t.push(1, false);
        t.taint();
        t.pop();
        t.finish_frame(false);
        assert_eq!(t.set(&[[0.0, 4.0], [0.0, 0.0]]), TransformUpdate::Moved);
        assert_eq!(
            t.set(&[[0.0, 0.0], [0.0, 4.0]]),
            TransformUpdate::NeedsUpload
        );
        assert_eq!(t.shift(1), [0.0, 0.0]);

        t.begin_frame(&[[0.0, 0.0]]);
        t.finish_frame(true);
        assert_eq!(t.set(&[[0.0, 4.0]]), TransformUpdate::NeedsUpload);
    }

    #[test]
    fn a_root_the_frame_did_not_paint_is_ignored() {
        let mut t = Transforms::default();
        t.begin_frame(&[]);
        t.finish_frame(false);
        assert_eq!(t.set(&[[3.0, 3.0]]), TransformUpdate::Unchanged);
    }
}
