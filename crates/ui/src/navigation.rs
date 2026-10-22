//! Navigation: the pages of a stack trading places.
//!
//! [`NodeStore::navigate`] shows one page of a stack in place of another,
//! moving the two as a [`PageTransition`] says: the page arriving moves in
//! from where it enters, the page leaving moves out to where it leaves and
//! hides once there. Both move as the presented values of animations
//! ([`NodeStore::animate`]), so neither lays out as it moves, and a
//! navigation while one is under way takes each page on from where it is —
//! a spring at the speed it has. A page leaving takes no pointer.

use core::time::Duration;

use crate::animation::{CubicBezier, Easing, LookTransition, LookValue, Run, Spring, Timing};
use crate::component::NodeStore;
use crate::layout::Vec2;
use crate::node::NodeId;

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
    /// Shows page `to` of a stack in place of page `from`, going `back` or
    /// forward, moving the two as `transition` says. `to` shows at once and
    /// `from` hides once it has moved out; a page moving from an earlier
    /// navigation moves on from where it is. With no `from`, or no
    /// transition, `to` simply takes `from`'s place. Pages stack in their
    /// order, so going forward the arriving page is above and going back
    /// the leaving one is. A stale `to` is a no-op.
    pub fn navigate(
        &mut self,
        from: Option<NodeId>,
        to: NodeId,
        transition: PageTransition,
        back: bool,
    ) {
        if !self.arena().is_live(to) {
            return;
        }
        let entering = self.hidden(to);
        self.set_hidden(to, false);
        self.leaving.retain(|&page| page != to);
        let Some(from) = from.filter(|&from| from != to && self.arena().is_live(from)) else {
            return;
        };
        let stack = self.arena().parent(to).unwrap_or(to);
        let width = self.bounds(stack).w.max(self.bounds(from).w);
        let rtl = self.direction_at(stack.index()) == crate::adaptive::LayoutDirection::Rtl;
        let Some(moves) = transition.moves(back, width, rtl, self.reduced_motion()) else {
            self.settle_page(to);
            self.settle_page(from);
            self.set_hidden(from, true);
            return;
        };
        let (dx, fade) = moves.into;
        self.move_page(to, dx, fade, (true, entering), moves.timing, None);
        let (dx, fade) = moves.out;
        let moved = self.move_page(from, dx, fade, (false, false), moves.timing, Some(from));
        // A leaving page that does not move hides with the arriving one's
        // arrival.
        if !moved && !self.move_page_hides(to, from) {
            self.set_hidden(from, true);
            return;
        }
        self.leaving.push(from);
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
                    for i in 0..3 {
                        let page = cx.leaf(LeafStyle {
                            size: Size::fill(),
                            ..Default::default()
                        });
                        cx.set_hidden(page, i != 0);
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
        store.layout(root, surface, &mut Vec::new());
        (store, root, [pages[0], pages[1], pages[2]])
    }

    fn ms(n: u64) -> Duration {
        Duration::from_millis(n)
    }

    #[test]
    fn a_slide_brings_the_page_over_from_the_trailing_edge() {
        let (mut store, _, [a, b, _]) = stack();
        store.navigate(Some(a), b, PageTransition::Slide, false);
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
        let (mut store, _, [a, b, _]) = stack();
        store.navigate(Some(a), b, PageTransition::Slide, false);
        store.tick_transitions(ms(80));
        let (bx, ax) = (store.translate(b).x, store.translate(a).x);
        store.navigate(Some(b), a, PageTransition::Slide, true);
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
        let (mut store, _, [a, b, c]) = stack();
        store.navigate(Some(a), b, PageTransition::Fade, false);
        assert_eq!(store.opacity(b), 0.0);
        store.tick_transitions(ms(100));
        assert_eq!(store.opacity(a), 1.0, "the page beneath stays opaque");
        assert!(store.opacity(b) > 0.0 && store.opacity(b) < 1.0);
        store.tick_transitions(ms(100));
        assert!(store.hidden(a) && store.opacity(b) == 1.0);
        store.navigate(Some(b), a, PageTransition::Fade, true);
        store.tick_transitions(ms(100));
        assert!(
            store.opacity(b) < 1.0 && store.opacity(a) == 1.0,
            "back, the top fades out"
        );
        store.tick_transitions(ms(100));
        assert!(store.hidden(b) && store.opacity(b) == 1.0);
        store.navigate(Some(a), c, PageTransition::None, false);
        assert!(store.hidden(a) && !store.hidden(c));
        assert!(!store.is_transitioning());
    }

    #[test]
    fn a_shared_axis_slides_and_fades_and_reduced_motion_makes_it_a_fade() {
        let (mut store, _, [a, b, _]) = stack();
        store.navigate(Some(a), b, PageTransition::SharedAxis, false);
        assert_eq!((store.translate(b).x, store.opacity(b)), (30.0, 0.0));
        store.tick_transitions(ms(150));
        assert!(store.translate(a).x < 0.0 && store.opacity(a) < 1.0);
        store.tick_transitions(ms(150));
        assert!(store.hidden(a));
        store.set_reduced_motion(true);
        store.navigate(Some(b), a, PageTransition::SharedAxis, true);
        assert_eq!(store.translate(a).x, 0.0, "no slide");
        store.tick_transitions(ms(100));
        assert!(store.opacity(b) < 1.0, "a fade still runs");
    }

    #[test]
    fn a_right_to_left_stack_slides_from_the_left() {
        let (mut store, root, [a, b, _]) = stack();
        store.set_layout_direction(root, Some(crate::adaptive::LayoutDirection::Rtl));
        store.navigate(Some(a), b, PageTransition::Slide, false);
        assert_eq!(store.translate(b).x, -400.0);
    }
}
