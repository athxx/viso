//! Transform roots in the primitive stream (§21).
//!
//! A [`Primitive::Transform`](crate::Primitive::Transform) bracket names the
//! transform root its content moves with. Geometry inside it is written as of
//! the frame it was painted, and each root has a *place*: the affine that maps
//! that geometry to where it draws now — the identity when the root is where it
//! was painted, a translation once a scroll step or a slide moved it, and a
//! scale or rotation about the root's origin when the root is turned, which no
//! paint can bake into axis-aligned instances. A later frame that changes only
//! places hands the new table to
//! [`Renderer::set_transforms`](crate::Renderer::set_transforms): each draw is
//! then mapped by its root's place through the view uniforms every built-in
//! reads, and each clip opened inside a root moves with it. No instance is
//! rewritten, no buffer is uploaded, and the cost is per draw, not per
//! primitive.
//!
//! A clip is recorded as a chain of links. A link is one layer clip pushed while
//! some root was open, together with that root, so its rect moves by that root's
//! place; the chain's first link also carries the static clip it was opened
//! under. Links are interned per frame, so siblings that open equal clips share
//! one chain and their draws still batch. A scissor only follows a translation:
//! a clip opened inside a turned root composites through an offscreen layer
//! instead, and a root whose scissored clips would turn needs a repaint.
//!
//! Content whose pixels are not a pure function of its own instances moved by a
//! place — an offscreen layer (its region of interest was cut against the
//! surface and its clip), a backdrop or material (they sample what is behind),
//! a user program (it writes its own uniforms) — taints the innermost open
//! root. Moving a tainted root, or moving anything while the frame captures a
//! backdrop, reports [`TransformUpdate::NeedsUpload`]: the caller repaints at
//! the new places and uploads, which is always correct.

use std::collections::HashMap;

use viso_math::Affine2;

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
    /// Roots moved; the next encode maps their draws to their new places.
    Moved,
    /// A moved root holds content its place cannot move: repaint at the new
    /// places and upload.
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
/// ingested, the clip links it recorded, and the place tables.
#[derive(Debug, Default)]
pub(crate) struct Transforms {
    /// The roots open at this point of the stream, innermost last.
    open: Vec<u32>,
    links: Vec<ClipLink>,
    interned: HashMap<LinkKey, u32>,
    /// Each root's place when the uploaded primitives were ingested.
    baked: Vec<Affine2>,
    /// Each root's place now.
    current: Vec<Affine2>,
    /// Whether a place moves each root's content correctly.
    plain: Vec<bool>,
    /// Whether a root holds a scissored clip, which only a translation moves.
    scissored: Vec<bool>,
    /// The frame captures a backdrop, which samples whatever moved under it.
    pinned: bool,
    /// Some root's current place differs from its baked one.
    moved: bool,
    /// Some root's current place is not the identity.
    displaced: bool,
    /// Bumped whenever an upload or a move changes where anything draws.
    epoch: u64,
}

impl Transforms {
    /// Start ingesting a stream whose roots are at `places`.
    pub(crate) fn begin_frame(&mut self, places: &[Affine2]) {
        self.open.clear();
        self.links.clear();
        self.interned.clear();
        self.baked.clear();
        self.baked.extend_from_slice(places);
        self.current.clear();
        self.current.extend_from_slice(places);
        self.plain.clear();
        self.plain.resize(places.len(), true);
        self.scissored.clear();
        self.scissored.resize(places.len(), false);
        self.pinned = false;
        self.moved = false;
        self.displaced = places.iter().any(|p| !p.is_identity());
        self.epoch += 1;
    }

    /// Where everything draws, as a counter: equal epochs, equal placement.
    pub(crate) fn epoch(&self) -> u64 {
        self.epoch
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
    /// pass's own space, which a place does not reach, so a root opened there is
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

    /// Mark the innermost open root as holding content a place cannot move.
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

    /// Mark the innermost open root as holding a clip its draws' scissor
    /// keeps.
    pub(crate) fn scissor(&mut self) {
        let slot = self.slot();
        if slot == NO_SLOT {
            return;
        }
        let i = slot as usize;
        if self.scissored.len() <= i {
            self.scissored.resize(i + 1, false);
        }
        self.scissored[i] = true;
    }

    /// Whether the innermost open root is turned: scaled or rotated, so a
    /// scissor cannot follow what it draws.
    pub(crate) fn turned(&self) -> bool {
        !self.place(self.slot()).is_translation()
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

    /// Take the roots' places now.
    pub(crate) fn set(&mut self, current: &[Affine2]) -> TransformUpdate {
        let stuck = current
            .iter()
            .zip(&self.baked)
            .enumerate()
            .any(|(slot, (now, then))| {
                now != then
                    && (self.pinned
                        || !self.plain.get(slot).copied().unwrap_or(true)
                        || (!now.is_translation()
                            && self.scissored.get(slot).copied().unwrap_or(false)))
            });
        if stuck {
            if self.moved {
                self.current.copy_from_slice(&self.baked);
                self.moved = false;
                self.displaced = self.baked.iter().any(|p| !p.is_identity());
                self.epoch += 1;
            }
            return TransformUpdate::NeedsUpload;
        }
        let mut moved = false;
        let mut changed = false;
        for (slot, then) in self.baked.iter().enumerate() {
            let now = current.get(slot).copied().unwrap_or(*then);
            moved |= now != *then;
            if self.current[slot] != now {
                self.current[slot] = now;
                changed = true;
            }
        }
        if changed {
            self.epoch += 1;
            self.displaced = self.current.iter().any(|p| !p.is_identity());
        }
        self.moved = moved;
        if moved {
            TransformUpdate::Moved
        } else {
            TransformUpdate::Unchanged
        }
    }

    /// Where root `slot`'s content draws now: the affine from where it was
    /// painted.
    pub(crate) fn place(&self, slot: u32) -> Affine2 {
        if !self.displaced || slot == NO_SLOT {
            return Affine2::IDENTITY;
        }
        self.current
            .get(slot as usize)
            .copied()
            .unwrap_or(Affine2::IDENTITY)
    }

    /// The clip of an emit in `space` now, given the clip it was painted with.
    /// Every link's root is at a translation: a clip in a turned root is no
    /// scissor.
    pub(crate) fn clip(&self, space: Space, painted: Option<Rect>) -> Option<Rect> {
        if !self.displaced || space.chain == NO_CHAIN {
            return painted;
        }
        let mut at = space.chain;
        let mut clip = Rect::INFINITE;
        while let Some(link) = self.links.get(at as usize) {
            let t = self.place(link.slot).translation();
            clip = clip.intersect(Rect {
                x: link.rect.x + t.x,
                y: link.rect.y + t.y,
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

    fn at(x: f32, y: f32) -> Affine2 {
        Affine2::translate(x, y)
    }

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
        t.begin_frame(&[Affine2::IDENTITY]);
        let viewport = Some(rect(0.0, 0.0, 100.0, 100.0));
        t.push(0, false);
        let chain = t.open_clip(NO_CHAIN, viewport, rect(0.0, 50.0, 100.0, 80.0));
        let space = t.space(chain);
        t.pop();
        t.finish_frame(false);
        assert_eq!(t.set(&[at(0.0, -30.0)]), TransformUpdate::Moved);
        assert_eq!(t.place(0), at(0.0, -30.0));
        assert_eq!(
            t.clip(space, Some(rect(0.0, 50.0, 100.0, 50.0))),
            Some(rect(0.0, 20.0, 100.0, 80.0))
        );
    }

    #[test]
    fn equal_sibling_clips_share_one_chain() {
        let mut t = Transforms::default();
        t.begin_frame(&[Affine2::IDENTITY]);
        t.push(0, false);
        let a = t.open_clip(NO_CHAIN, None, rect(0.0, 0.0, 5.0, 5.0));
        let b = t.open_clip(NO_CHAIN, None, rect(0.0, 0.0, 5.0, 5.0));
        assert_eq!(a, b);
    }

    #[test]
    fn moving_a_tainted_root_or_under_a_capture_needs_an_upload() {
        let mut t = Transforms::default();
        t.begin_frame(&[Affine2::IDENTITY; 2]);
        t.push(1, false);
        t.taint();
        t.pop();
        t.finish_frame(false);
        assert_eq!(
            t.set(&[at(0.0, 4.0), Affine2::IDENTITY]),
            TransformUpdate::Moved
        );
        assert_eq!(
            t.set(&[Affine2::IDENTITY, at(0.0, 4.0)]),
            TransformUpdate::NeedsUpload
        );
        assert_eq!(t.place(1), Affine2::IDENTITY);

        t.begin_frame(&[Affine2::IDENTITY]);
        t.finish_frame(true);
        assert_eq!(t.set(&[at(0.0, 4.0)]), TransformUpdate::NeedsUpload);
    }

    #[test]
    fn a_root_the_frame_did_not_paint_is_ignored() {
        let mut t = Transforms::default();
        t.begin_frame(&[]);
        t.finish_frame(false);
        assert_eq!(t.set(&[at(3.0, 3.0)]), TransformUpdate::Unchanged);
    }

    #[test]
    fn a_scissored_root_that_turns_needs_an_upload() {
        let mut t = Transforms::default();
        t.begin_frame(&[Affine2::IDENTITY]);
        t.push(0, false);
        assert!(!t.turned());
        t.scissor();
        t.pop();
        t.finish_frame(false);
        assert_eq!(t.set(&[at(5.0, 0.0)]), TransformUpdate::Moved);
        let turned = Affine2::from_rotation(0.5);
        assert_eq!(t.set(&[turned]), TransformUpdate::NeedsUpload);

        t.begin_frame(&[turned]);
        t.push(0, false);
        assert!(t.turned(), "a clip opened here cannot be a scissor");
        t.pop();
        t.finish_frame(false);
        let more = Affine2::from_rotation(0.7);
        assert_eq!(t.set(&[more]), TransformUpdate::Moved);
        assert_eq!(t.place(0), more);
    }
}
