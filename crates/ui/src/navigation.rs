//! Navigation: a stack showing one of its pages, and the pages trading
//! places.
//!
//! A paged stack ([`NodeStore::set_page`]) shows the child at its page index
//! and folds every other child out of layout, paint, hit testing and the
//! semantics tree, as hiding each would — a child linked later folds too, and
//! one its own `visible` hides stays hidden. Turned to another page, it moves
//! the two as its [`PageTransition`] says: the page arriving moves in from
//! where it enters, the page leaving moves out to where it leaves and folds
//! once there. Both move as the presented values of animations
//! ([`NodeStore::animate`]), so neither lays out as it moves, and a turn
//! while one is under way takes each page on from where it is — a spring at
//! the speed it has. A page leaving takes no pointer. The table is cold: a
//! store with no paged stack pays one emptiness check where a node's
//! visibility is read.

use core::time::Duration;

use crate::animation::{CubicBezier, Easing, LookTransition, LookValue, Run, Spring, Timing};
use crate::component::NodeStore;
use crate::dirty::DirtyClass;
use crate::layout::Vec2;
use crate::node::{NodeId, NodeIndex};

/// The paged stacks, each with the index of the child it shows. Cold and
/// small: a handful of navigation stacks at most.
#[derive(Debug, Default)]
pub(crate) struct Paged {
    stacks: Vec<(NodeId, Option<usize>)>,
}

impl Paged {
    pub(crate) fn clear(&mut self) {
        self.stacks.clear();
    }

    /// Forgets the slot `index`, freed or reused.
    pub(crate) fn forget(&mut self, index: u32) {
        self.stacks.retain(|(n, _)| n.index() != index);
    }

    fn at(&self, index: NodeIndex) -> Option<Option<usize>> {
        self.stacks
            .iter()
            .find(|(n, _)| n.index() == index.get())
            .map(|&(_, page)| page)
    }
}

/// How the pages of a stack trade places.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum PageTransition {
    /// The one this platform's own navigation makes: a slide on Apple's, a
    /// shared axis on Android, a fade elsewhere.
    #[default]
    Native,
    /// The arriving page slides over from the trailing edge, the one it
    /// covers drifting a third of the way the other way; back, the reverse.
    Slide,
    /// Both slide a little along the axis of travel, the leaving page fading
    /// out as the arriving one fades in.
    SharedAxis,
    /// The page on top fades: in over the other going forward, out going
    /// back.
    Fade,
    /// The pages trade places at once.
    None,
}

/// How a page moves through a navigation: where it starts or ends, as an
/// offset across its stack in dp and an opacity, and over what timing.
#[derive(Debug, Clone, Copy, PartialEq)]
struct Moves {
    /// The arriving page's start, `None` for where it rests.
    into: (Option<f32>, Option<f32>),
    /// The leaving page's end, `None` for where it rests.
    out: (Option<f32>, Option<f32>),
    timing: Timing,
}

/// The share of its stack's width a covered page drifts.
const DRIFT: f32 = 0.3;
/// How far (dp) a shared axis slides.
const AXIS: f32 = 30.0;

impl PageTransition {
    /// The transition this platform's navigation makes.
    pub fn native() -> PageTransition {
        if cfg!(any(target_os = "macos", target_os = "ios")) {
            PageTransition::Slide
        } else if cfg!(target_os = "android") {
            PageTransition::SharedAxis
        } else {
            PageTransition::Fade
        }
    }

    /// How the two pages move going `back` or forward across a stack
    /// `width` wide, its trailing edge leftward when `rtl`; under reduced
    /// motion a move across becomes a fade. `None` for none.
    fn moves(self, back: bool, width: f32, rtl: bool, reduced: bool) -> Option<Moves> {
        let kind = match self {
            PageTransition::Native => PageTransition::native(),
            PageTransition::Slide | PageTransition::SharedAxis if reduced => PageTransition::Fade,
            kind => kind,
        };
        // Toward the trailing edge.
        let on = if rtl { -1.0 } else { 1.0 };
        let fade = Timing::eased(Duration::from_millis(200), Easing::EASE_IN_OUT)
            .reduced(crate::animation::ReducedMotion::Keep);
        Some(match kind {
            PageTransition::Slide => {
                let timing = Timing::spring(Spring::with_response(0.5, 1.0));
                let (full, drift) = (width * on, -width * DRIFT * on);
                if back {
                    Moves {
                        into: (Some(drift), None),
                        out: (Some(full), None),
                        timing,
                    }
                } else {
                    Moves {
                        into: (Some(full), None),
                        out: (Some(drift), None),
                        timing,
                    }
                }
            }
            PageTransition::SharedAxis => {
                let timing = Timing::eased(
                    Duration::from_millis(300),
                    Easing::CubicBezier(CubicBezier::new(0.2, 0.0, 0.0, 1.0)),
                );
                let step = if back { -AXIS * on } else { AXIS * on };
                Moves {
                    into: (Some(step), Some(0.0)),
                    out: (Some(-step), Some(0.0)),
                    timing,
                }
            }
            // The page on top fades, so the stack never shows through.
            PageTransition::Fade if back => Moves {
                into: (None, None),
                out: (None, Some(0.0)),
                timing: fade,
            },
            PageTransition::Fade => Moves {
                into: (None, Some(0.0)),
                out: (None, None),
                timing: fade,
            },
            PageTransition::None | PageTransition::Native => return None,
        })
    }
}

impl NodeStore {
    /// Shows the child of `stack` at `page`, none for `None`, folding every
    /// other child away; turned from another page, the two move as
    /// `transition` says — back toward a lower page, else forward. Pages
    /// stack in their order, so going forward the arriving page is above
    /// and going back the leaving one is. The first page a stack is given
    /// shows at once. A stale handle is a no-op.
    pub fn set_page(&mut self, stack: NodeId, page: Option<usize>, transition: PageTransition) {
        if !self.arena().is_live(stack) {
            return;
        }
        let Some(at) = self.paged.stacks.iter().position(|&(n, _)| n == stack) else {
            self.paged.stacks.push((stack, page));
            let mut child = self.arena().first_child(stack);
            while let Some(c) = child {
                child = self.arena().next_sibling(c);
                self.shown_changed(c);
            }
            self.mark_dirty(
                stack,
                DirtyClass::MEASURE
                    | DirtyClass::LAYOUT
                    | DirtyClass::PAINT
                    | DirtyClass::SEMANTICS,
            );
            return;
        };
        let was = self.paged.stacks[at].1;
        if was == page {
            return;
        }
        let from = was.and_then(|i| self.page_at(stack, i));
        let to = page.and_then(|i| self.page_at(stack, i));
        let back = matches!((was, page), (Some(a), Some(b)) if b < a);
        // A page turned back to as it leaves moves on from where it is.
        let entering = to.is_some_and(|to| !self.is_leaving(to));
        if let Some(to) = to {
            self.leaving.retain(|&p| p != to);
        }
        let moves = (from.is_some() && to.is_some())
            .then(|| {
                let width = self.bounds(stack).w;
                let rtl = self.direction_at(stack.index()) == crate::adaptive::LayoutDirection::Rtl;
                transition.moves(back, width, rtl, self.reduced_motion())
            })
            .flatten();
        match (moves, from, to) {
            (Some(moves), Some(from), Some(to)) => {
                // The leaving page shows on as it moves out.
                self.leaving.retain(|&p| p != from);
                self.leaving.push(from);
                let (dx, fade) = moves.into;
                self.move_page(to, dx, fade, (true, entering), moves.timing, None);
                let (dx, fade) = moves.out;
                let moved =
                    self.move_page(from, dx, fade, (false, false), moves.timing, Some(from));
                // A leaving page that does not move folds with the arriving
                // one's arrival.
                if !moved && !self.move_page_hides(to, from) {
                    self.leaving.retain(|&p| p != from);
                }
            }
            _ => {
                for page in [from, to].into_iter().flatten() {
                    self.settle_page(page);
                    self.leaving.retain(|&p| p != page);
                }
            }
        }
        self.paged.stacks[at].1 = page;
        for page in [from, to].into_iter().flatten() {
            self.shown_changed(page);
        }
    }

    /// The index of the page `stack` shows, `None` for none or for a stack
    /// that is not paged.
    pub fn shown_page(&self, stack: NodeId) -> Option<usize> {
        self.paged
            .stacks
            .iter()
            .find(|&&(n, _)| n == stack)
            .and_then(|&(_, page)| page)
    }

    /// Whether `stack` is a paged stack.
    pub fn is_paged(&self, stack: NodeId) -> bool {
        self.paged.stacks.iter().any(|&(n, _)| n == stack)
    }

    /// The paged stack a system back action turns back: the deepest one
    /// past its first page that shows, its ancestors included. `None` for
    /// none.
    pub fn back_target(&self) -> Option<NodeId> {
        self.paged
            .stacks
            .iter()
            .filter(|&&(stack, page)| {
                page.is_some_and(|p| p > 0) && self.arena().is_live(stack) && self.showing(stack)
            })
            .map(|&(stack, _)| stack)
            .max_by_key(|&stack| self.depth_of(stack))
    }

    /// Whether `id` and each of its ancestors shows.
    fn showing(&self, id: NodeId) -> bool {
        let mut at = Some(id);
        while let Some(node) = at {
            if self.hidden(node) {
                return false;
            }
            at = self.arena().parent(node);
        }
        true
    }

    /// The child of `stack` at `index`.
    fn page_at(&self, stack: NodeId, index: usize) -> Option<NodeId> {
        self.arena().children(stack).nth(index)
    }

    /// Whether the slot `index` is a child of a paged stack other than the
    /// one it shows and no page leaving it.
    #[inline]
    pub(crate) fn paged_away(&self, index: NodeIndex) -> bool {
        if self.paged.stacks.is_empty() {
            return false;
        }
        let arena = self.arena();
        let parent = arena.parent_of(index);
        if parent.is_none() {
            return false;
        }
        let Some(page) = self.paged.at(parent) else {
            return false;
        };
        let mut child = arena.first_child_of(parent);
        let mut at = 0;
        while !child.is_none() && child != index {
            child = arena.next_sibling_of(child);
            at += 1;
        }
        page != Some(at) && !self.leaving.iter().any(|p| p.index() == index.get())
    }

    /// Whether `page` is leaving its stack: it takes no pointer.
    #[inline]
    pub(crate) fn is_leaving(&self, page: NodeId) -> bool {
        !self.leaving.is_empty() && self.leaving.contains(&page)
    }

    /// Ends the page moves of `page` where they rest.
    fn settle_page(&mut self, page: NodeId) {
        for property in [LookValue::Translate(Vec2::ZERO), LookValue::Opacity(1.0)] {
            if let Some(at) = self
                .animations
                .iter()
                .position(|a| a.motion.moves(page, &property))
            {
                let ended = self.animations.swap_remove(at);
                self.end_animation(ended, false);
            }
        }
    }

    /// Moves `page` as it arrives or leaves, as `(arriving, entering)` say:
    /// arriving, from its offset `dx` across its stack and its opacity `fade`
    /// when it is `entering` (else from where it is) back to where it rests;
    /// leaving, from where it is to those, or where it rests where `None`.
    /// Each moves over `timing`; the first move started hides `hides` once
    /// it arrives. Whether one did.
    fn move_page(
        &mut self,
        page: NodeId,
        dx: Option<f32>,
        fade: Option<f32>,
        (arriving, entering): (bool, bool),
        timing: Timing,
        mut hides: Option<NodeId>,
    ) -> bool {
        let mut hiding = false;
        let rest = self.shown(page, &LookValue::Translate(Vec2::ZERO));
        let LookValue::Translate(at) = rest else {
            return false;
        };
        let across = |dx: f32| {
            LookValue::Translate(Vec2 {
                x: at.x + dx,
                y: at.y,
            })
        };
        let opaque = self.shown(page, &LookValue::Opacity(1.0));
        let moves = [
            (rest, dx.map(across)),
            (opaque, fade.map(LookValue::Opacity)),
        ];
        for (rest, end) in moves {
            let (start, target) = match (arriving, entering) {
                (true, true) => (end, rest),
                (true, false) => (None, rest),
                (false, _) => (None, end.unwrap_or(rest)),
            };
            let presented = self.presented(page, &rest);
            let moving = self.animations.iter().any(|a| a.motion.moves(page, &rest));
            if !moving && start.unwrap_or(presented) == target {
                continue;
            }
            let instant = self.lands(&timing);
            let started = self.run_animation(
                page,
                rest,
                (instant, timing.reduced),
                None,
                |presented, carried| {
                    Some(Run::Move(
                        LookTransition::new(page, start.unwrap_or(presented), target, timing)
                            .carrying(carried),
                    ))
                },
            );
            if started.is_some()
                && !instant
                && let Some(running) = self.animations.last_mut()
                && running.motion.node() == page
            {
                running.quiet = true;
                if let Some(hide) = hides.take() {
                    running.hides = Some(hide);
                    hiding = true;
                }
                if let Some(start) = start {
                    self.show_look(page, start);
                }
            }
        }
        hiding
    }

    /// Makes the last move started on `page` hide `hide` once it arrives;
    /// whether `page` has one.
    fn move_page_hides(&mut self, page: NodeId, hide: NodeId) -> bool {
        match self
            .animations
            .iter_mut()
            .rev()
            .find(|a| a.motion.node() == page)
        {
            Some(running) => {
                running.hides = Some(hide);
                true
            }
            None => false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::component::{BuildCx, LeafStyle, StackStyle};
    use crate::layout::Size;
    use viso_render::Rect;

    /// A 400 × 300 stack of three filled pages, the first shown.
    fn stack() -> (NodeStore, NodeId, [NodeId; 3]) {
        let mut store = NodeStore::new();
        let mut pages = Vec::new();
        let root = {
            let mut cx = BuildCx::new(&mut store);
            cx.stack(
                StackStyle {
                    size: Size::fixed(400.0, 300.0),
                    ..Default::default()
                },
                |cx| {
                    for _ in 0..3 {
                        let page = cx.leaf(LeafStyle {
                            size: Size::fill(),
                            ..Default::default()
                        });
                        pages.push(page.id());
                    }
                },
            );
            cx.root().unwrap()
        };
        let surface = Rect {
            x: 0.0,
            y: 0.0,
            w: 400.0,
            h: 300.0,
        };
        store.set_page(root, Some(0), PageTransition::None);
        store.layout(root, surface, &mut Vec::new());
        (store, root, [pages[0], pages[1], pages[2]])
    }

    fn ms(n: u64) -> Duration {
        Duration::from_millis(n)
    }

    #[test]
    fn a_slide_brings_the_page_over_from_the_trailing_edge() {
        let (mut store, root, [a, b, _]) = stack();
        store.set_page(root, Some(1), PageTransition::Slide);
        assert!(
            !store.hidden(b) && !store.hidden(a),
            "both show as they move"
        );
        assert_eq!(store.translate(b).x, 400.0, "it starts past the edge");
        assert!(store.is_leaving(a) && !store.is_leaving(b));
        store.tick_transitions(ms(100));
        let (bx, ax) = (store.translate(b).x, store.translate(a).x);
        assert!(bx > 0.0 && bx < 400.0, "{bx}");
        assert!(
            ax < 0.0 && ax > -120.0,
            "the covered page drifts back: {ax}"
        );
        for _ in 0..200 {
            store.tick_transitions(ms(16));
        }
        assert_eq!(store.translate(b).x, 0.0);
        assert!(store.hidden(a), "the covered page hides once there");
        assert_eq!(store.translate(a).x, 0.0, "and rests where it was");
        assert!(!store.is_leaving(a));
        assert!(
            store.take_animation_ends().is_empty(),
            "a page's moves are no animations its watchers hear"
        );
    }

    #[test]
    fn going_back_mid_slide_takes_each_page_on_from_where_it_is() {
        let (mut store, root, [a, b, _]) = stack();
        store.set_page(root, Some(1), PageTransition::Slide);
        store.tick_transitions(ms(80));
        let (bx, ax) = (store.translate(b).x, store.translate(a).x);
        store.set_page(root, Some(0), PageTransition::Slide);
        assert!(!store.hidden(a), "the page it was leaving stays");
        store.tick_transitions(ms(4));
        assert!(
            (store.translate(b).x - bx).abs() < 20.0,
            "no jump: {bx} then {}",
            store.translate(b).x
        );
        assert!((store.translate(a).x - ax).abs() < 20.0);
        for _ in 0..200 {
            store.tick_transitions(ms(16));
        }
        assert!(store.hidden(b) && !store.hidden(a));
        assert_eq!(store.translate(a).x, 0.0);
    }

    #[test]
    fn a_fade_fades_the_page_on_top_and_none_swaps_at_once() {
        let (mut store, root, [a, b, c]) = stack();
        store.set_page(root, Some(1), PageTransition::Fade);
        assert_eq!(store.opacity(b), 0.0);
        store.tick_transitions(ms(100));
        assert_eq!(store.opacity(a), 1.0, "the page beneath stays opaque");
        assert!(store.opacity(b) > 0.0 && store.opacity(b) < 1.0);
        store.tick_transitions(ms(100));
        assert!(store.hidden(a) && store.opacity(b) == 1.0);
        store.set_page(root, Some(0), PageTransition::Fade);
        store.tick_transitions(ms(100));
        assert!(
            store.opacity(b) < 1.0 && store.opacity(a) == 1.0,
            "back, the top fades out"
        );
        store.tick_transitions(ms(100));
        assert!(store.hidden(b) && store.opacity(b) == 1.0);
        store.set_page(root, Some(2), PageTransition::None);
        assert!(store.hidden(a) && !store.hidden(c));
        assert!(!store.is_transitioning());
    }

    #[test]
    fn a_shared_axis_slides_and_fades_and_reduced_motion_makes_it_a_fade() {
        let (mut store, root, [a, b, _]) = stack();
        store.set_page(root, Some(1), PageTransition::SharedAxis);
        assert_eq!((store.translate(b).x, store.opacity(b)), (30.0, 0.0));
        store.tick_transitions(ms(150));
        assert!(store.translate(a).x < 0.0 && store.opacity(a) < 1.0);
        store.tick_transitions(ms(150));
        assert!(store.hidden(a));
        store.set_reduced_motion(true);
        store.set_page(root, Some(0), PageTransition::SharedAxis);
        assert_eq!(store.translate(a).x, 0.0, "no slide");
        store.tick_transitions(ms(100));
        assert!(store.opacity(b) < 1.0, "a fade still runs");
    }

    #[test]
    fn a_right_to_left_stack_slides_from_the_left() {
        let (mut store, root, [_, b, _]) = stack();
        store.set_layout_direction(root, Some(crate::adaptive::LayoutDirection::Rtl));
        store.set_page(root, Some(1), PageTransition::Slide);
        assert_eq!(store.translate(b).x, -400.0);
    }

    #[test]
    fn a_paged_stack_folds_all_but_its_page_and_respects_a_page_s_own_hiding() {
        let (mut store, root, [a, b, c]) = stack();
        assert!(!store.hidden(a) && store.hidden(b) && store.hidden(c));
        assert_eq!(store.shown_page(root), Some(0));
        store.set_hidden(b, true);
        store.set_page(root, Some(1), PageTransition::None);
        assert!(store.hidden(a), "the page it left folds");
        assert!(store.hidden(b), "a page hidden itself stays hidden");
        store.set_hidden(b, false);
        assert!(!store.hidden(b));
        store.set_page(root, None, PageTransition::Slide);
        assert!(store.hidden(a) && store.hidden(b) && store.hidden(c));
        assert!(!store.is_transitioning(), "to no page nothing moves");
    }

    #[test]
    fn a_back_action_turns_the_deepest_showing_stack_past_its_first_page() {
        let (mut store, root, [a, _, _]) = stack();
        assert_eq!(store.back_target(), None, "on its first page");
        store.set_page(root, Some(2), PageTransition::None);
        assert_eq!(store.back_target(), Some(root));
        store.set_hidden(root, true);
        assert_eq!(store.back_target(), None, "a stack that does not show");
        store.set_hidden(root, false);
        // A stack paged within the first page, which does not show.
        store.set_page(a, Some(1), PageTransition::None);
        assert_eq!(store.back_target(), Some(root));
        store.set_page(root, Some(0), PageTransition::None);
        assert_eq!(store.back_target(), Some(a), "the deeper one shows now");
    }
}
