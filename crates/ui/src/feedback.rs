//! Press feedback: the ink ripple a press spreads over a control, and the
//! haptic a control plays as a finger changes it.
//!
//! A node that ripples ([`NodeStore::set_ripple`]) shows, while pressed, a
//! circle of ink spreading from where the press landed until it covers the
//! box, clipped to the box's rounded corners and drawn over its fill, under
//! its content; let go, the ink fades as it spreads on. The ink is the box's
//! own contrast: dark over a light or clear fill, light over a dark one. The
//! ripples of a store run on the frame loop only while one shows; under
//! reduced motion the ink covers the box at once and only fades.
//!
//! Interactive glass lights from within instead: the same press, timed the
//! same way, is a glow its glass draws from where it landed, spreading
//! through the glass and any glass melted into it.
//!
//! A haptic is asked for through [`EventCx::feedback`](crate::EventCx::feedback)
//! and played once the frame is done: a frame plays one, the last asked.
//! The table of rippling nodes is cold: one entry for each node told, kept
//! in slot order, and the ripples showing are a short list.

use std::time::Duration;

use viso_render::{AnalyticEllipse, Border, Corners, LayerClip, Primitive, Rect, Rgba};

use crate::component::NodeStore;
use crate::dirty::DirtyClass;
use crate::node::NodeId;

/// A haptic pattern, named by intent; each platform plays its closest.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Haptic {
    /// A selection moved: a toggle flipped, a segment or a detent picked.
    Selection,
    Light,
    Medium,
    Heavy,
    Success,
    Warning,
    Error,
}

/// Whether the platform's controls ripple when pressed: Android's do.
pub const NATIVE_RIPPLE: bool = cfg!(target_os = "android");

/// How long the ink takes to cover the box, and to fade once let go
/// (seconds).
const SPREAD: f32 = 0.3;
const FADE: f32 = 0.2;
/// How bright a press's glow inside glass is at its peak.
const GLOW: f32 = 0.35;

/// How strong the ink is over a light fill, and over a dark one.
const DARK_INK: f32 = 0.12;
const LIGHT_INK: f32 = 0.24;

/// One ripple showing.
#[derive(Debug, Clone, Copy, PartialEq)]
struct Ripple {
    node: NodeId,
    /// Where the press landed, from the node's top-left corner.
    from: [f32; 2],
    /// How long it has shown, and how long it had when let go (seconds).
    age: f32,
    released: Option<f32>,
}

impl Ripple {
    /// How far it has spread, `0..=1` of the way to covering the box.
    fn spread(&self, reduced: bool) -> f32 {
        if reduced {
            return 1.0;
        }
        let t = (self.age / SPREAD).min(1.0);
        1.0 - (1.0 - t).powi(3)
    }

    /// How strong it shows, `0..=1`.
    fn strength(&self) -> f32 {
        self.released
            .map_or(1.0, |at| (1.0 - (self.age - at) / FADE).max(0.0))
    }

    /// Whether it is held, already covering the box: it shows unchanged
    /// until let go.
    fn settled(&self, reduced: bool) -> bool {
        self.released.is_none() && (reduced || self.age >= SPREAD)
    }

    fn done(&self) -> bool {
        // A frame's rounding short of the fade still ends it.
        self.released.is_some_and(|at| self.age - at >= FADE - 1e-3)
    }
}

/// The nodes told to ripple or not, the ripples showing, and the haptic the
/// frame plays.
#[derive(Debug, Default)]
pub(crate) struct Feedback {
    nodes: Vec<(u32, bool)>,
    showing: Vec<Ripple>,
    haptic: Option<Haptic>,
}

impl Feedback {
    pub(crate) fn clear(&mut self) {
        *self = Feedback::default();
    }

    /// Forgets the slot `index`, freed or reused.
    pub(crate) fn forget(&mut self, index: u32) {
        if let Ok(at) = self.find(index) {
            self.nodes.remove(at);
        }
        self.showing.retain(|r| r.node.index() != index);
    }

    fn find(&self, index: u32) -> Result<usize, usize> {
        self.nodes.binary_search_by_key(&index, |e| e.0)
    }

    fn ripples(&self, index: u32) -> bool {
        !self.nodes.is_empty() && self.find(index).is_ok_and(|at| self.nodes[at].1)
    }
}

impl NodeStore {
    /// Whether a press on node `id` spreads ink over it.
    pub fn ripples(&self, id: NodeId) -> bool {
        self.feedback.ripples(id.index())
    }

    /// Makes a press on node `id` spread ink over it, or not. A live-guarded
    /// write — a stale handle is a no-op.
    pub fn set_ripple(&mut self, id: NodeId, on: bool) {
        if !self.arena().is_live(id) || self.ripples(id) == on {
            return;
        }
        let index = id.index();
        let nodes = &mut self.feedback.nodes;
        match nodes.binary_search_by_key(&index, |e| e.0) {
            Ok(at) => nodes[at].1 = on,
            Err(at) => nodes.insert(at, (index, on)),
        }
        if !on {
            self.feedback.showing.retain(|r| r.node != id);
            self.mark_dirty(id, DirtyClass::PAINT);
        }
    }

    /// Starts the ripple of a press at `(x, y)` in the window on `target`:
    /// on the nearest node from it up that ripples.
    pub(crate) fn start_ripple(&mut self, target: NodeId, x: f32, y: f32) {
        if self.feedback.nodes.is_empty() && !self.has_glass() {
            return;
        }
        let mut at = Some(target);
        while let Some(node) = at {
            if self.ripples(node) || self.glows(node) {
                let world = self.world(node);
                self.feedback.showing.push(Ripple {
                    node,
                    from: [x - world.x, y - world.y],
                    age: 0.0,
                    released: None,
                });
                self.mark_dirty(node, DirtyClass::PAINT);
                if let Some(parent) = self.parent(node)
                    && self.glass_merge(parent) > 0.0
                {
                    self.mark_dirty(parent, DirtyClass::PAINT);
                }
                return;
            }
            at = self.parent(node);
        }
    }

    /// Whether a press on node `id` lights its glass from within.
    #[inline]
    fn glows(&self, id: NodeId) -> bool {
        self.backdrop(id).is_some_and(|b| b.interactive)
    }

    /// The glow a press on node `id` lights its glass with, over its box
    /// `world`: from where it landed, reaching further as it spreads.
    pub(crate) fn press_glow(&self, id: NodeId, world: Rect) -> Option<viso_render::Glow> {
        let showing = &self.feedback.showing;
        if showing.is_empty() {
            return None;
        }
        let ripple = showing.iter().rev().find(|r| r.node == id)?;
        let [x, y] = ripple.from;
        let reach = x.max(world.w - x).hypot(y.max(world.h - y));
        let spread = ripple.spread(self.reduced_motion());
        Some(viso_render::Glow {
            at: viso_render::Point::new(world.x + x, world.y + y),
            radius: reach * (0.4 + 0.6 * spread),
            strength: GLOW * ripple.strength(),
        })
    }

    /// Lets go of the ripples a press holds: they fade as they spread on.
    pub(crate) fn release_ripples(&mut self) {
        let mut i = self.feedback.showing.len();
        while i > 0 {
            i -= 1;
            let ripple = &mut self.feedback.showing[i];
            if ripple.released.is_none() {
                ripple.released = Some(ripple.age);
                // A ripple held still wakes the frame loop to fade.
                let node = ripple.node;
                if self.arena().is_live(node) {
                    self.mark_dirty(node, DirtyClass::PAINT);
                }
            }
        }
    }

    /// Whether any ripple shows, spreading, held or fading.
    #[inline]
    pub(crate) fn shows_ripples(&self) -> bool {
        !self.feedback.showing.is_empty()
    }

    /// Whether a ripple spreads or fades: the frame loop beats while one
    /// does. One held over the whole box waits for its release.
    #[inline]
    pub fn is_rippling(&self) -> bool {
        let showing = &self.feedback.showing;
        !showing.is_empty() && {
            let reduced = self.reduced_motion();
            showing.iter().any(|r| !r.settled(reduced))
        }
    }

    /// Runs the ripples on by a frame of `delta`, repainting their nodes and
    /// dropping the ones faded out.
    pub fn tick_ripples(&mut self, delta: Duration) {
        let step = delta.as_secs_f32().min(0.1);
        let reduced = self.reduced_motion();
        let mut i = self.feedback.showing.len();
        while i > 0 {
            i -= 1;
            let ripple = &mut self.feedback.showing[i];
            if ripple.settled(reduced) {
                continue;
            }
            ripple.age += step;
            let (node, done) = (ripple.node, ripple.done());
            if done || !self.arena().is_live(node) {
                self.feedback.showing.swap_remove(i);
            }
            if self.arena().is_live(node) {
                self.mark_dirty(node, DirtyClass::PAINT);
            }
        }
    }

    /// Asks for `haptic` to play once the frame is done.
    pub fn request_haptic(&mut self, haptic: Haptic) {
        self.feedback.haptic = Some(haptic);
    }

    /// The haptic asked for since the last call: the driver plays it.
    pub fn take_haptic(&mut self) -> Option<Haptic> {
        self.feedback.haptic.take()
    }

    /// Appends node `node`'s ripples over its box `world` rounded by
    /// `radius`, over its fill `fill`.
    pub(crate) fn paint_ripples(
        &self,
        node: NodeId,
        world: Rect,
        radius: f32,
        fill: Rgba,
        out: &mut Vec<Primitive>,
    ) {
        let showing = &self.feedback.showing;
        if showing.is_empty() || !showing.iter().any(|r| r.node == node) || self.glows(node) {
            return;
        }
        let reduced = self.reduced_motion();
        out.push(Primitive::Layer(LayerClip {
            clip: world,
            opacity: 1.0,
            blur_sigma: 0.0,
            backdrop_sigma: 0.0,
        }));
        if radius > 0.0 {
            out.push(Primitive::ClipCorners(Corners::uniform(radius)));
        }
        let ink = ink(fill);
        for ripple in showing.iter().filter(|r| r.node == node) {
            let [x, y] = ripple.from;
            // Far enough to reach the corner farthest from the press.
            let reach = x.max(world.w - x).hypot(y.max(world.h - y));
            let r = reach * ripple.spread(reduced);
            let (cx, cy) = (world.x + x, world.y + y);
            out.push(Primitive::AnalyticEllipse(AnalyticEllipse {
                rect: Rect::new(cx - r, cy - r, 2.0 * r, 2.0 * r),
                color: Rgba {
                    a: ink.a * ripple.strength(),
                    ..ink
                },
                border: Border::NONE,
            }));
        }
        out.push(Primitive::LayerEnd);
    }
}

/// The ink over a box filled with `fill`: dark over a light or clear fill,
/// light over a dark one.
fn ink(fill: Rgba) -> Rgba {
    let luma = 0.2126 * fill.r + 0.7152 * fill.g + 0.0722 * fill.b;
    if fill.a >= 0.5 && luma < 0.18 {
        Rgba {
            r: 1.0,
            g: 1.0,
            b: 1.0,
            a: LIGHT_INK,
        }
    } else {
        Rgba {
            r: 0.0,
            g: 0.0,
            b: 0.0,
            a: DARK_INK,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::component::{BuildCx, LeafStyle};
    use crate::layout::Sizing;
    use crate::style::BoxStyle;

    fn button(store: &mut NodeStore) -> NodeId {
        let id = {
            let mut cx = BuildCx::new(store);
            cx.leaf(LeafStyle {
                size: Sizing::fixed(100.0, 40.0),
                style: BoxStyle::solid(Rgba::new(1.0, 1.0, 1.0, 1.0)).with_radius(8.0),
            })
            .id()
        };
        store.layout(id, Rect::new(0.0, 0.0, 400.0, 300.0), &mut Vec::new());
        id
    }

    fn ink_drawn(store: &NodeStore, root: NodeId) -> Vec<AnalyticEllipse> {
        let mut out = Vec::new();
        crate::paint::paint_tree(store, root, &mut out);
        out.iter()
            .filter_map(|p| match p {
                Primitive::AnalyticEllipse(e) => Some(*e),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn a_press_spreads_ink_that_fades_once_let_go() {
        let mut store = NodeStore::new();
        let id = button(&mut store);
        store.start_ripple(id, 10.0, 20.0);
        assert!(!store.is_rippling(), "a node not told to ripple shows none");

        store.set_ripple(id, true);
        store.start_ripple(id, 10.0, 20.0);
        assert!(store.is_rippling());
        store.tick_ripples(Duration::from_millis(150));
        let first = ink_drawn(&store, id)[0];
        let centre = (
            first.rect.x + first.rect.w * 0.5,
            first.rect.y + first.rect.h * 0.5,
        );
        assert_eq!(centre, (10.0, 20.0), "it spreads from the press");
        assert_eq!(
            first.color,
            Rgba::new(0.0, 0.0, 0.0, DARK_INK),
            "dark ink on white"
        );

        store.tick_ripples(Duration::from_millis(100));
        store.tick_ripples(Duration::from_millis(100));
        assert!(!store.is_rippling(), "held over the box, it waits");
        let covering = ink_drawn(&store, id)[0];
        let world = store.world(id);
        let reach = (world.w - 10.0).hypot(world.h - 20.0);
        assert!(
            (covering.rect.w * 0.5 - reach).abs() < 1e-3,
            "it covers the box"
        );

        store.release_ripples();
        assert!(store.is_rippling(), "let go, it moves again");
        store.tick_ripples(Duration::from_millis(100));
        assert!(
            ink_drawn(&store, id)[0].color.a < DARK_INK,
            "let go, it fades"
        );
        store.tick_ripples(Duration::from_millis(100));
        assert!(!store.is_rippling(), "and goes");
        assert!(ink_drawn(&store, id).is_empty());
    }

    #[test]
    fn a_frame_plays_the_last_haptic_asked() {
        let mut store = NodeStore::new();
        assert_eq!(store.take_haptic(), None);
        store.request_haptic(Haptic::Light);
        store.request_haptic(Haptic::Selection);
        assert_eq!(store.take_haptic(), Some(Haptic::Selection));
        assert_eq!(store.take_haptic(), None);
    }
}
