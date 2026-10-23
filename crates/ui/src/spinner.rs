//! Spinners: a node's drawing that says work is under way.
//!
//! A spinner is a node's content ([`Content::Spinner`]): spokes round a hub
//! (Apple systems) or an arc (Material). Before it spins it shows how near
//! the work is to starting: the spokes appear one by one round the hub, the
//! arc grows with an arrow at its head, as a pull toward a refresh nears its
//! mark. Spinning, the spokes fade behind a lead that steps round eight times
//! a second, by the node's rotation alone (a timer steps it; no frame beats
//! between steps), and the arc chases its tail round the ring, repainted each
//! frame ([`NodeStore::tick_spinners`]).

use std::f32::consts::{PI, TAU};
use std::time::Duration;

use viso_render::{LineCap, Path, PathCmd, Point, Primitive, Rect, Rgba, Stroke};

use crate::component::{BuildCx, Handle, LeafStyle, NodeStore};
use crate::content::Content;
use crate::dirty::DirtyClass;
use crate::layout::{Alignment2D, Size, Vec2};
use crate::node::NodeId;
use crate::style::BoxStyle;
use crate::timer::TimerRequest;

/// How a spinner draws.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SpinnerLook {
    /// Eight spokes round a hub, a lead stepping round them (Apple).
    Spokes,
    /// An arc chasing its tail round a ring (Material).
    Arc,
}

impl SpinnerLook {
    /// This system's spinner.
    pub fn native() -> SpinnerLook {
        if cfg!(any(target_os = "macos", target_os = "ios")) {
            SpinnerLook::Spokes
        } else {
            SpinnerLook::Arc
        }
    }
}

/// What a spinner node draws.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Spinner {
    pub look: SpinnerLook,
    pub color: Rgba,
    /// How much of it shows before it spins, `0` to `1`: the spokes drawn,
    /// the arc's reach.
    pub progress: f32,
    /// Whether it spins.
    pub spinning: bool,
    /// How long it has spun (seconds): where an arc's chase stands.
    pub time: f32,
    /// Its side, the size it measures to.
    pub natural: Vec2,
}

const SPOKES: usize = 8;
/// How long the lead stays on a spoke.
const STEP: Duration = Duration::from_millis(125);
/// The arc's chase: one grow-and-shrink, how far the arc reaches at most
/// (turns), and how far the ring turns over it.
const CYCLE: f32 = 1.332;
const REACH: f32 = 0.8;
const CYCLE_TURN: f32 = 0.6;

impl Spinner {
    /// A still spinner of `look`, `side` across, inked `color`, showing in
    /// full.
    pub fn new(look: SpinnerLook, side: f32, color: Rgba) -> Spinner {
        Spinner {
            look,
            color,
            progress: 1.0,
            spinning: false,
            time: 0.0,
            natural: Vec2 { x: side, y: side },
        }
    }

    /// Draws it into `rect` (world space).
    pub(crate) fn paint(&self, rect: Rect, out: &mut Vec<Primitive>) {
        let side = rect.w.min(rect.h);
        if side <= 0.0 || self.color.a <= 0.0 {
            return;
        }
        let center = Point {
            x: rect.x + rect.w * 0.5,
            y: rect.y + rect.h * 0.5,
        };
        match self.look {
            SpinnerLook::Spokes => self.paint_spokes(center, side, out),
            SpinnerLook::Arc => self.paint_arc(center, side, out),
        }
    }

    fn paint_spokes(&self, center: Point, side: f32, out: &mut Vec<Primitive>) {
        let width = side * 0.09;
        let (inner, outer) = (side * 0.24 + width * 0.5, side * 0.5 - width * 0.5);
        let shown = if self.spinning {
            SPOKES
        } else {
            (self.progress.clamp(0.0, 1.0) * SPOKES as f32).round() as usize
        };
        for j in 0..shown {
            // Clockwise from the top; spinning, those behind the lead fade.
            let alpha = if self.spinning {
                1.0 - ((SPOKES - j) % SPOKES) as f32 * 0.1
            } else {
                1.0
            };
            let (sin, cos) = (j as f32 * TAU / SPOKES as f32 - PI * 0.5).sin_cos();
            let at = |r: f32| Point {
                x: center.x + r * cos,
                y: center.y + r * sin,
            };
            let mut stroke = Stroke::new(width, faded(self.color, alpha));
            stroke.cap = LineCap::Round;
            out.push(Primitive::Path(Box::new(Path {
                cmds: vec![PathCmd::MoveTo(at(inner)), PathCmd::LineTo(at(outer))],
                fill: None,
                stroke: Some(stroke),
                shadow: None,
            })));
        }
    }

    fn paint_arc(&self, center: Point, side: f32, out: &mut Vec<Primitive>) {
        let width = side * 0.1;
        let radius = side * 0.5 - width * 0.5;
        // Turns clockwise from the top.
        let (from, to, arrow) = if self.spinning {
            let (from, to) = chase(self.time, width / (TAU * radius));
            (from, to, 0.0)
        } else {
            // A pull's reach: the arc starts growing at two fifths of it.
            let grown = ((self.progress.clamp(0.0, 1.0) - 0.4).max(0.0) * 5.0 / 3.0).min(1.0);
            let turn = (-0.25 + 0.4 * grown) * 0.5;
            (turn, turn + grown * REACH, grown)
        };
        if to - from <= 0.0 {
            return;
        }
        let mut cmds = Vec::with_capacity(14);
        arc(&mut cmds, center, radius, from * TAU, to * TAU);
        let mut stroke = Stroke::new(width, self.color);
        stroke.cap = LineCap::Square;
        out.push(Primitive::Path(Box::new(Path {
            cmds,
            fill: None,
            stroke: Some(stroke),
            shadow: None,
        })));
        if arrow > 0.0 {
            // An arrowhead at the head, pointing on round the ring.
            let (sin, cos) = (to * TAU - PI * 0.5).sin_cos();
            let (half, ahead) = (width * 2.0 * arrow, width * 2.0 * arrow);
            let on = |r: f32, t: f32| Point {
                x: center.x + r * cos - t * sin,
                y: center.y + r * sin + t * cos,
            };
            out.push(Primitive::Path(Box::new(Path {
                cmds: vec![
                    PathCmd::MoveTo(on(radius - half, 0.0)),
                    PathCmd::LineTo(on(radius + half, 0.0)),
                    PathCmd::LineTo(on(radius, ahead)),
                    PathCmd::Close,
                ],
                fill: Some(self.color),
                stroke: None,
                shadow: None,
            })));
        }
    }
}

/// Where an arc chasing its tail round a ring stands `time` seconds in, at
/// least `least` long: its tail and head, in turns clockwise from the top.
pub(crate) fn chase(time: f32, least: f32) -> (f32, f32) {
    let cycles = time / CYCLE;
    let (k, u) = (cycles.floor(), cycles.fract());
    let base = k * (REACH - least);
    let head = base + least + (REACH - least) * fast_out_slow_in((u * 2.0).min(1.0));
    let tail = base + (REACH - least) * fast_out_slow_in((u * 2.0 - 1.0).max(0.0));
    let turn = (CYCLE_TURN * cycles + base).fract() - base;
    (tail + turn, head + turn)
}

/// `color` at `alpha` of its opacity.
fn faded(color: Rgba, alpha: f32) -> Rgba {
    Rgba {
        a: color.a * alpha,
        ..color
    }
}

/// An arc of the circle about `center` of `radius`, from `from` to `to`
/// (radians clockwise from the top), as cubics of at most a quarter turn.
pub(crate) fn arc(cmds: &mut Vec<PathCmd>, center: Point, radius: f32, from: f32, to: f32) {
    let at = |a: f32| {
        let (sin, cos) = (a - PI * 0.5).sin_cos();
        (sin, cos)
    };
    let point = |(sin, cos): (f32, f32)| Point {
        x: center.x + radius * cos,
        y: center.y + radius * sin,
    };
    let pieces = ((to - from) / (PI * 0.5)).ceil().max(1.0);
    let step = (to - from) / pieces;
    let k = 4.0 / 3.0 * (step / 4.0).tan() * radius;
    let mut a = from;
    let mut start = at(a);
    cmds.push(PathCmd::MoveTo(point(start)));
    for _ in 0..pieces as usize {
        let end = at(a + step);
        let (p0, p3) = (point(start), point(end));
        cmds.push(PathCmd::CubicTo(
            Point {
                x: p0.x - k * start.0,
                y: p0.y + k * start.1,
            },
            Point {
                x: p3.x + k * end.0,
                y: p3.y - k * end.1,
            },
            p3,
        ));
        a += step;
        start = end;
    }
}

/// Material's standard easing, `cubic-bezier(0.4, 0, 0.2, 1)`, at `x`.
pub(crate) fn fast_out_slow_in(x: f32) -> f32 {
    let bezier = |p1: f32, p2: f32, t: f32| {
        let u = 1.0 - t;
        3.0 * u * u * t * p1 + 3.0 * u * t * t * p2 + t * t * t
    };
    // The curve's x rises with t: halve toward the t whose x is `x`.
    let (mut lo, mut hi) = (0.0f32, 1.0f32);
    for _ in 0..20 {
        let mid = (lo + hi) * 0.5;
        if bezier(0.4, 0.2, mid) < x {
            lo = mid;
        } else {
            hi = mid;
        }
    }
    bezier(0.0, 1.0, (lo + hi) * 0.5)
}

/// The spinning spinners of a store: the arcs, repainted each frame, and the
/// spokes, stepped by a timer, each with the chain its timer belongs to.
#[derive(Debug, Default)]
pub(crate) struct Spinners {
    arcs: Vec<NodeId>,
    spokes: Vec<(NodeId, u32)>,
    chains: u32,
}

impl Spinners {
    pub(crate) fn clear(&mut self) {
        self.arcs.clear();
        self.spokes.clear();
    }

    /// Forgets the spinner of the slot `index`, freed or reused.
    pub(crate) fn forget(&mut self, index: u32) {
        self.arcs.retain(|id| id.index() != index);
        self.spokes.retain(|(id, _)| id.index() != index);
    }
}

impl BuildCx<'_> {
    /// A spinner leaf of `look`, `side` across, inked `color`, still and in
    /// full; [`NodeStore::set_spinning`] spins it.
    pub fn spinner(&mut self, look: SpinnerLook, side: f32, color: Rgba) -> Handle {
        let handle = self.leaf(LeafStyle {
            size: Size::fixed(side, side),
            style: BoxStyle::NONE,
        });
        let id = handle.id();
        self.store
            .set_content_payload(id, Content::Spinner(Spinner::new(look, side, color)));
        self.store.set_transform_origin(id, Alignment2D::Center);
        self.store.set_hittable(id, false);
        handle
    }

    /// Spins the spinner `handle` names, as [`NodeStore::set_spinning`].
    pub fn spin(&mut self, handle: Handle) {
        self.store.set_spinning(handle.id(), true);
    }
}

impl NodeStore {
    /// Node `id`'s spinner, if it draws one.
    pub fn spinner(&self, id: NodeId) -> Option<Spinner> {
        match self.content_payload(id) {
            Some(Content::Spinner(spinner)) => Some(*spinner),
            _ => None,
        }
    }

    /// Change node `id`'s spinner by `change`, repainting it when that
    /// changed anything.
    fn change_spinner(&mut self, id: NodeId, change: impl FnOnce(&mut Spinner)) {
        let Some(spinner) = self.spinner_mut(id) else {
            return;
        };
        let was = *spinner;
        change(spinner);
        if *spinner != was {
            self.mark_dirty(id, DirtyClass::PAINT);
        }
    }

    /// Show `progress` (`0` to `1`) of node `id`'s still spinner.
    pub fn set_spinner_progress(&mut self, id: NodeId, progress: f32) {
        self.change_spinner(id, |s| s.progress = progress.clamp(0.0, 1.0));
    }

    /// Ink node `id`'s spinner `color`.
    pub fn set_spinner_color(&mut self, id: NodeId, color: Rgba) {
        self.change_spinner(id, |s| s.color = color);
    }

    /// Spin node `id`'s spinner, or stop it. Stopped, it stands still as it
    /// started.
    pub fn set_spinning(&mut self, id: NodeId, on: bool) {
        let Some(spinner) = self.spinner(id) else {
            return;
        };
        if spinner.spinning == on {
            return;
        }
        self.change_spinner(id, |s| {
            s.spinning = on;
            s.time = 0.0;
        });
        let spinners = &mut self.spinners;
        spinners.arcs.retain(|&a| a != id);
        spinners.spokes.retain(|&(s, _)| s != id);
        if !on {
            if spinner.look == SpinnerLook::Spokes {
                self.set_rotation(id, 0.0);
            }
            return;
        }
        match spinner.look {
            SpinnerLook::Arc => spinners.arcs.push(id),
            SpinnerLook::Spokes => {
                spinners.chains = spinners.chains.wrapping_add(1);
                let chain = spinners.chains;
                spinners.spokes.push((id, chain));
                self.queue_step(id, chain, 1);
            }
        }
    }

    /// Arm the timer that moves spokes `id`'s lead to spoke `step`.
    fn queue_step(&mut self, id: NodeId, chain: u32, step: usize) {
        self.queue_timer(TimerRequest::new(id, STEP, move |store| {
            if !store.spinners.spokes.contains(&(id, chain)) {
                return;
            }
            // Spokes out of sight keep their step without moving.
            if store.shows(id) {
                store.set_rotation(id, (step % SPOKES) as f32 * TAU / SPOKES as f32);
            }
            store.queue_step(id, chain, (step + 1) % SPOKES);
        }));
    }

    /// Whether an arc that shows spins: the frame loop beats while one does.
    #[inline]
    pub fn is_spinning(&self) -> bool {
        self.spinners.arcs.iter().any(|&id| self.shows(id))
    }

    /// Advance every spinning arc by a frame of `delta`, repainting it.
    pub fn tick_spinners(&mut self, delta: Duration) {
        let dt = delta.as_secs_f32().min(0.1);
        let mut i = self.spinners.arcs.len();
        while i > 0 {
            i -= 1;
            let id = self.spinners.arcs[i];
            if self.spinner(id).is_none() {
                self.spinners.arcs.swap_remove(i);
                continue;
            }
            if self.shows(id) {
                self.change_spinner(id, |s| s.time += dt);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use viso_render::Rect as RenderRect;

    const WHITE: Rgba = Rgba::new(1.0, 1.0, 1.0, 1.0);

    fn painted(spinner: &Spinner) -> Vec<Primitive> {
        let mut out = Vec::new();
        spinner.paint(
            RenderRect {
                x: 10.0,
                y: 10.0,
                w: 20.0,
                h: 20.0,
            },
            &mut out,
        );
        out
    }

    #[test]
    fn spokes_appear_one_by_one_and_fade_behind_the_lead_spinning() {
        let mut s = Spinner::new(SpinnerLook::Spokes, 20.0, WHITE);
        s.progress = 0.0;
        assert!(painted(&s).is_empty());
        s.progress = 0.5;
        assert_eq!(painted(&s).len(), 4);
        s.spinning = true;
        let alphas: Vec<f32> = painted(&s)
            .iter()
            .map(|p| match p {
                Primitive::Path(path) => path.stroke.unwrap().color.a,
                _ => unreachable!(),
            })
            .collect();
        assert_eq!(alphas.len(), 8);
        assert_eq!(alphas[0], 1.0, "the lead at the top");
        assert!(alphas[7] > alphas[6] && alphas[1] < alphas[2], "{alphas:?}");
    }

    #[test]
    fn an_arc_grows_with_its_pull_and_chases_its_tail_spinning() {
        let mut s = Spinner::new(SpinnerLook::Arc, 20.0, WHITE);
        s.progress = 0.3;
        assert!(painted(&s).is_empty(), "nothing before two fifths");
        s.progress = 1.0;
        assert_eq!(painted(&s).len(), 2, "the arc and its arrowhead");
        s.spinning = true;
        for t in [0.0, 0.3, 0.7, 1.2, 5.0] {
            s.time = t;
            assert_eq!(painted(&s).len(), 1, "no arrow spinning");
        }
        assert!((fast_out_slow_in(0.5) - 0.7756).abs() < 0.01);
        assert!(fast_out_slow_in(0.0).abs() < 1e-4);
        assert!((fast_out_slow_in(1.0) - 1.0).abs() < 1e-4);
    }

    #[test]
    fn spinning_spokes_step_by_rotation_and_an_arc_by_frames() {
        let mut store = NodeStore::new();
        let mut cx = BuildCx::new(&mut store);
        let spokes = cx.spinner(SpinnerLook::Spokes, 20.0, WHITE).id();
        let ring = cx.spinner(SpinnerLook::Arc, 20.0, WHITE).id();
        store.set_spinning(spokes, true);
        assert!(!store.is_spinning(), "spokes beat no frames");
        store.set_spinning(ring, true);
        assert!(store.is_spinning());
        store.clear_dirty();
        store.tick_spinners(Duration::from_millis(16));
        assert!(store.dirty(ring).contains(DirtyClass::PAINT));
        assert!((store.spinner(ring).unwrap().time - 0.016).abs() < 1e-6);
        store.set_spinning(ring, false);
        assert!(!store.is_spinning());
        assert_eq!(store.spinner(ring).unwrap().time, 0.0);
    }
}
