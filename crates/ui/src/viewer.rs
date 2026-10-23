//! Viewers: a node whose one content child pans and zooms within it.
//!
//! A viewer ([`NodeStore::set_viewer`]) shows its content scaled about the
//! content's top-left corner and moved by a pan, both written as the child's
//! transform, so a pan or a zoom marks only `TRANSFORM`. A drag pans it, a
//! mouse's as a finger's, and a pinch zooms it about the pinch's middle while
//! the middle's travel pans it: two fingers, a trackpad's pinch, or a wheel
//! turned with Control held. A wheel or a trackpad's two-finger scroll pans it
//! before any viewport above. Arrow and page keys pan the viewer the keyboard
//! is in, `+` and `-` zoom it about its middle, and `0` or Home put it back.
//!
//! The content keeps within bounds: along each axis, content larger than the
//! viewer keeps its edges at or past the viewer's, and smaller content stays
//! in the middle; a margin lets it move that much further either way. The
//! scale keeps within the limits. Where the platform's viewports bounce
//! ([`OverscrollStyle::Bounce`]), a finger, the trackpad's fingers or a pinch
//! may pull past the bounds or the limits, ever more stiffly, and once let go
//! it springs back. A drag that lifts moving coasts on, each axis along the
//! platform's fling curve, bouncing at the bounds where viewports bounce. A
//! touch drag the content cannot follow along the way it set out is left to a
//! viewport above that can, so a photo at rest in a pager pages.
//!
//! A move asked for ([`NodeStore::move_viewer`]) — back to rest, a zoom about
//! a point, a pan, a double tap's toggle — glides there, the scale evenly in
//! proportion and the point it zooms about straight to where it lands; under
//! reduced motion it lands at once.

use core::time::Duration;

use crate::component::NodeStore;
use crate::drag::{DragPhase, DragSample};
use crate::input::{Key, KeyEvent, PointerId};
use crate::key_scroll::LINE_STEP;
use crate::layout::{Alignment2D, Axis, OnAxis, Vec2};
use crate::node::NodeId;
use crate::pinch::{PinchPhase, PinchSample};
use crate::scroll_motion::{
    CATCH_VELOCITY, MAX_STEP, Motion, OverscrollStyle, PULL_COEFF, PULL_RANGE, SPRING,
    VelocityTracker,
};
use crate::transform::Turn;

/// How far one press of `+` or `-` zooms.
const KEY_ZOOM: f32 = 1.25;
/// The share of the viewer a page key pans.
const PAGE_SHARE: f32 = 0.875;
/// How far (in natural log) a pinch shows the scale past a limit at most.
const SCALE_PULL: f32 = 0.4;
/// The rate (1/s) a move asked for eases along: most of the way in a
/// quarter second, there in about half of one.
const GLIDE: f64 = 14.0;
/// Within this distance (points) and under this speed (pt/s) a spring back
/// has arrived.
const SPRING_NEAR: f64 = 0.25;
const SPRING_STILL: f64 = 10.0;

/// How far a viewer's content may scale and move.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ViewerLimits {
    /// The least scale it shows at; `1` is as laid out.
    pub min_scale: f32,
    /// The greatest scale it shows at.
    pub max_scale: f32,
    /// How far past its bounds it may move, on every side; infinite for a
    /// pan with no bounds.
    pub margin: f32,
}

impl Default for ViewerLimits {
    fn default() -> Self {
        ViewerLimits {
            min_scale: 1.0,
            max_scale: 4.0,
            margin: 0.0,
        }
    }
}

impl ViewerLimits {
    /// The limits made sound: scales positive and in order, the margin not
    /// negative.
    fn sound(self) -> ViewerLimits {
        let min = if self.min_scale.is_finite() && self.min_scale > 0.0 {
            self.min_scale
        } else {
            1.0
        };
        let max = if self.max_scale.is_nan() {
            min
        } else {
            self.max_scale.max(min)
        };
        ViewerLimits {
            min_scale: min,
            max_scale: max,
            margin: if self.margin.is_nan() {
                0.0
            } else {
                self.margin.max(0.0)
            },
        }
    }

    /// The scale it rests at: as laid out, within the limits.
    fn rest(&self) -> f32 {
        1.0f32.clamp(self.min_scale, self.max_scale)
    }
}

/// A move of a viewer's content asked for ([`NodeStore::move_viewer`]).
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum ViewerMove {
    /// Back as laid out: the resting scale, unmoved.
    Reset,
    /// To `scale`, keeping what shows at window point `at` there; the
    /// viewer's middle for `None`.
    Zoom { scale: f32, at: Option<Vec2> },
    /// By `factor`, about `at` as [`Zoom`](Self::Zoom).
    ZoomBy { factor: f32, at: Option<Vec2> },
    /// The content moved by `by`.
    Pan(Vec2),
    /// To `scale` about window point `at` while at rest, back to rest when
    /// zoomed in: a double tap.
    Toggle { scale: f32, at: Vec2 },
}

/// One axis of a viewer's pan after a release.
#[derive(Debug, Clone, Copy)]
enum Run {
    Still,
    /// Coasting on along the fling curve.
    Coast(Motion),
    /// Springing back within the bounds, moving at this speed (pt/s).
    Spring(f64),
}

/// A move gliding: the content point `q` (content units) goes from shown
/// at `p0` to shown at `p1` (pan space) while the scale goes from `s0` to
/// `s1` evenly in proportion, `t` seconds in.
#[derive(Debug, Clone, Copy)]
struct Glide {
    s0: f32,
    s1: f32,
    q: Vec2,
    p0: Vec2,
    p1: Vec2,
    t: f64,
}

impl Glide {
    /// The scale and pan it ends at.
    fn end(&self) -> (f32, Vec2) {
        (self.s1, self.p1 - self.q * self.s1)
    }
}

/// One viewer: the scale its content shows at and the pan that moves the
/// content's top-left corner from where it was laid out (pan space).
#[derive(Debug, Clone, Copy)]
struct Viewer {
    node: NodeId,
    limits: ViewerLimits,
    scale: f32,
    pan: Vec2,
    runs: [Run; 2],
    glide: Option<Glide>,
    /// The middle of the pinch under way, for its speed as it ends.
    track: VelocityTracker,
}

impl Viewer {
    fn moving(&self) -> bool {
        self.glide.is_some() || self.runs.iter().any(|r| !matches!(r, Run::Still))
    }

    fn halt(&mut self) {
        self.glide = None;
        self.runs = [Run::Still; 2];
    }

    /// The scale and pan it heads for: a glide's end, else where it is.
    fn aim(&self) -> (f32, Vec2) {
        self.glide.map_or((self.scale, self.pan), |g| g.end())
    }
}

/// A window's viewers. Cold: a store with none spends nothing on them.
#[derive(Debug, Default)]
pub(crate) struct Viewers {
    list: Vec<Viewer>,
}

impl Viewers {
    pub(crate) fn clear(&mut self) {
        self.list.clear();
    }

    /// Whether a viewer coasts, springs back or glides.
    pub(crate) fn moving(&self) -> bool {
        self.list.iter().any(Viewer::moving)
    }
}

/// How far a pull of `raw` past an end shows when it shows `d` at most.
fn eased(raw: f32, d: f32) -> f32 {
    if d <= 0.0 {
        return 0.0;
    }
    (raw.abs() * PULL_COEFF * d / (raw.abs() * PULL_COEFF + d)).copysign(raw)
}

/// The pull that shows as `shown` past an end, as [`eased`] shows it.
fn uneased(shown: f32, d: f32) -> f32 {
    if d <= 0.0 {
        return 0.0;
    }
    let s = shown.abs().min(d * 0.999);
    (s * d / (PULL_COEFF * (d - s))).copysign(shown)
}

/// Where a value shown at `at` moved by `by` shows within `(lo, hi)`: what
/// passes either end shows eased, `d` past it at most; with `d` zero it
/// stops at the end.
fn pulled(at: f32, by: f32, (lo, hi): (f32, f32), d: f32) -> f32 {
    let raw = if at > hi {
        hi + uneased(at - hi, d)
    } else if at < lo {
        lo + uneased(at - lo, d)
    } else {
        at
    };
    let next = raw + by;
    if next > hi {
        hi + eased(next - hi, d)
    } else if next < lo {
        lo + eased(next - lo, d)
    } else {
        next
    }
}

fn on(v: &mut Vec2, axis: Axis) -> &mut f32 {
    match axis {
        Axis::Row => &mut v.x,
        Axis::Column => &mut v.y,
    }
}

const AXES: [Axis; 2] = [Axis::Row, Axis::Column];

impl NodeStore {
    /// Make node `id` a viewer of its one content child within `limits`,
    /// or no longer one with `None`: it takes drags and pinches, and its
    /// content shows as laid out until moved.
    pub fn set_viewer(&mut self, id: NodeId, limits: Option<ViewerLimits>) {
        let was = self.viewer_index(id);
        let mut list = std::mem::take(&mut self.viewers.list);
        list.retain(|v| v.node != id && self.arena().is_live(v.node));
        self.viewers.list = list;
        if was.is_some()
            && limits.is_none()
            && let Some(content) = self.viewer_content(id)
        {
            self.set_turn(content, Turn::NONE);
            self.set_translate(content, Vec2::ZERO);
        }
        let on = limits.is_some() && self.arena().is_live(id);
        self.set_draggable(id, on);
        self.set_pinchable(id, on);
        if let (true, Some(limits)) = (on, limits) {
            self.viewers.list.push(Viewer {
                node: id,
                limits: limits.sound(),
                scale: limits.sound().rest(),
                pan: Vec2::ZERO,
                runs: [Run::Still; 2],
                glide: None,
                track: VelocityTracker::default(),
            });
            let i = self.viewers.list.len() - 1;
            self.show_viewer(i);
        }
    }

    /// Whether node `id` is a viewer.
    pub fn is_viewer(&self, id: NodeId) -> bool {
        self.viewer_index(id).is_some()
    }

    /// The scale viewer `id`'s content shows at.
    pub fn viewer_scale(&self, id: NodeId) -> Option<f32> {
        self.viewer_index(id).map(|i| self.viewers.list[i].scale)
    }

    /// How far viewer `id`'s content is moved from where it was laid out:
    /// its top-left corner's shift.
    pub fn viewer_pan(&self, id: NodeId) -> Option<Vec2> {
        self.viewer_index(id).map(|i| self.viewers.list[i].pan)
    }

    /// Move viewer `id`'s content as `to` asks, within its bounds and
    /// limits, gliding there (at once under reduced motion). A move on from
    /// one gliding heads on from where that one was headed.
    pub fn move_viewer(&mut self, id: NodeId, to: ViewerMove) {
        let Some(i) = self.viewer_index(id) else {
            return;
        };
        let Some(content) = self.viewer_content(id) else {
            return;
        };
        let v = self.viewers.list[i];
        let (scale, pan) = v.aim();
        let focus = |store: &NodeStore, at: Option<Vec2>| match at {
            Some(at) => store.pan_point(id, content, at),
            None => store.viewer_middle(id, content),
        };
        let rest = v.limits.rest();
        let to = match to {
            ViewerMove::Toggle { .. } if scale > rest * 1.01 => ViewerMove::Reset,
            ViewerMove::Toggle { scale, at } => ViewerMove::Zoom {
                scale,
                at: Some(at),
            },
            other => other,
        };
        match to {
            ViewerMove::Reset => {
                let f = self.viewer_middle(id, content);
                self.glide_viewer(i, content, rest, f, Some(Vec2::ZERO));
            }
            ViewerMove::Zoom { scale: s, at } => {
                let f = focus(self, at);
                self.glide_viewer(i, content, s, f, None);
            }
            ViewerMove::ZoomBy { factor, at } => {
                let f = focus(self, at);
                self.glide_viewer(i, content, scale * factor, f, None);
            }
            ViewerMove::Pan(by) => {
                let f = self.viewer_middle(id, content);
                self.glide_viewer(i, content, scale, f, Some(pan + by));
            }
            ViewerMove::Toggle { .. } => {}
        }
    }

    fn viewer_index(&self, id: NodeId) -> Option<usize> {
        self.viewers.list.iter().position(|v| v.node == id)
    }

    /// The nearest viewer at or above `node`.
    fn viewer_over(&self, node: NodeId) -> Option<usize> {
        if self.viewers.list.is_empty() {
            return None;
        }
        let mut at = Some(node);
        while let Some(n) = at {
            if let Some(i) = self.viewer_index(n) {
                return Some(i);
            }
            at = self.arena().parent(n);
        }
        None
    }

    /// Viewer `id`'s content: its first child that is not chrome.
    fn viewer_content(&self, id: NodeId) -> Option<NodeId> {
        self.arena().children(id).find(|&c| !self.is_chrome(c))
    }

    /// Window point `at` in viewer `id`'s pan space: from where its
    /// `content`'s top-left corner was laid out.
    fn pan_point(&self, id: NodeId, content: NodeId, at: Vec2) -> Vec2 {
        let (vb, cb) = (self.bounds(id), self.bounds(content));
        let local = self.to_local(id, at.x, at.y);
        Vec2::new(vb.x + local.x - cb.x, vb.y + local.y - cb.y)
    }

    /// Viewer `id`'s middle in its pan space.
    fn viewer_middle(&self, id: NodeId, content: NodeId) -> Vec2 {
        let (vb, cb) = (self.bounds(id), self.bounds(content));
        Vec2::new(vb.x + vb.w * 0.5 - cb.x, vb.y + vb.h * 0.5 - cb.y)
    }

    /// A window motion `by` seen at `at` in viewer `id`'s layout space.
    fn viewer_motion(&self, id: NodeId, at: Vec2, by: Vec2) -> Vec2 {
        let to = self.to_local(id, at.x, at.y);
        let from = self.to_local(id, at.x - by.x, at.y - by.y);
        to - from
    }

    /// The least and greatest pan of viewer `i` along `axis` at `scale`.
    fn viewer_span(&self, i: usize, content: NodeId, axis: Axis, scale: f32) -> (f32, f32) {
        let v = &self.viewers.list[i];
        let (vb, cb) = (self.bounds(v.node), self.bounds(content));
        let (v0, ve, c0, ce) = match axis {
            Axis::Row => (vb.x, vb.w, cb.x, cb.w),
            Axis::Column => (vb.y, vb.h, cb.y, cb.h),
        };
        let shown = ce * scale;
        let (lo, hi) = if shown >= ve {
            (v0 + ve - shown, v0)
        } else {
            let mid = v0 + (ve - shown) * 0.5;
            (mid, mid)
        };
        let m = v.limits.margin;
        (lo - m - c0, hi + m - c0)
    }

    /// How far viewer `i` shows pulled past its bounds along `axis` at
    /// most: none where the platform's viewports do not bounce.
    fn viewer_pull(&self, i: usize, axis: Axis) -> f32 {
        if self.scroll_physics().overscroll != OverscrollStyle::Bounce {
            return 0.0;
        }
        let b = self.bounds(self.viewers.list[i].node);
        let extent = match axis {
            Axis::Row => b.w,
            Axis::Column => b.h,
        };
        extent * PULL_RANGE
    }

    /// Writes viewer `i`'s scale and pan as its content's transform.
    fn show_viewer(&mut self, i: usize) {
        let v = self.viewers.list[i];
        let Some(content) = self.viewer_content(v.node) else {
            return;
        };
        // The content scales about its top-left corner, whichever way it
        // reads.
        let origin = if self.direction_at(content.index()) == crate::adaptive::LayoutDirection::Rtl
        {
            Alignment2D::TopEnd
        } else {
            Alignment2D::TopStart
        };
        self.set_turn(
            content,
            Turn {
                scale: v.scale,
                rotation: 0.0,
                origin,
            },
        );
        self.set_translate(content, v.pan);
    }

    /// Pans viewer `i` by `by` (layout space), pulled past its bounds as
    /// far as `elastic` lets it.
    fn pan_viewer(&mut self, i: usize, content: NodeId, by: Vec2, elastic: bool) {
        for axis in AXES {
            let d = by.on(axis);
            if d == 0.0 {
                continue;
            }
            let span = self.viewer_span(i, content, axis, self.viewers.list[i].scale);
            let pull = if elastic {
                self.viewer_pull(i, axis)
            } else {
                0.0
            };
            let v = &mut self.viewers.list[i];
            let at = on(&mut v.pan, axis);
            *at = if pull > 0.0 {
                pulled(*at, d, span, pull)
            } else {
                (*at + d).clamp(span.0, span.1)
            };
        }
    }

    /// Scales viewer `i` by `factor` about `focus` (pan space), pulled past
    /// its limits as far as `elastic` lets it; else its pan keeps within
    /// the bounds the new scale sets.
    fn zoom_viewer(&mut self, i: usize, content: NodeId, factor: f32, focus: Vec2, elastic: bool) {
        let elastic = elastic && self.scroll_physics().overscroll == OverscrollStyle::Bounce;
        let v = &mut self.viewers.list[i];
        let span = (v.limits.min_scale.ln(), v.limits.max_scale.ln());
        let s = v.scale;
        let next = if elastic {
            pulled(s.ln(), factor.ln(), span, SCALE_PULL)
        } else {
            (s.ln() + factor.ln()).clamp(span.0, span.1)
        }
        .exp();
        let k = next / s;
        v.scale = next;
        v.pan = focus - (focus - v.pan) * k;
        if !elastic {
            self.clamp_viewer(i, content);
        }
    }

    /// Brings viewer `i`'s pan within its bounds at once.
    fn clamp_viewer(&mut self, i: usize, content: NodeId) {
        for axis in AXES {
            let span = self.viewer_span(i, content, axis, self.viewers.list[i].scale);
            let at = on(&mut self.viewers.list[i].pan, axis);
            *at = at.clamp(span.0, span.1);
        }
    }

    /// Glides viewer `i` to `scale` (within its limits) keeping what shows
    /// at `focus` (pan space) there, or to pan `pan` with the focus heading
    /// where that puts it; the pan keeps within the bounds at that scale.
    fn glide_viewer(
        &mut self,
        i: usize,
        content: NodeId,
        scale: f32,
        focus: Vec2,
        pan: Option<Vec2>,
    ) {
        let v = self.viewers.list[i];
        let s1 = if scale.is_finite() {
            scale.clamp(v.limits.min_scale, v.limits.max_scale)
        } else {
            v.scale
        };
        let q = (focus - v.pan) * (1.0 / v.scale);
        let mut p1 = pan.unwrap_or(focus - q * s1);
        for axis in AXES {
            let (lo, hi) = self.viewer_span(i, content, axis, s1);
            let at = on(&mut p1, axis);
            *at = at.clamp(lo, hi);
        }
        let glide = Glide {
            s0: v.scale,
            s1,
            q,
            p0: focus,
            p1: p1 + q * s1,
            t: 0.0,
        };
        let instant = self.reduced_motion();
        let v = &mut self.viewers.list[i];
        v.halt();
        if instant {
            (v.scale, v.pan) = glide.end();
        } else if (glide.s1 - glide.s0).abs() > 1e-4 || (glide.p1 - glide.p0).length() > 0.25 {
            v.glide = Some(glide);
        } else {
            (v.scale, v.pan) = glide.end();
        }
        self.show_viewer(i);
    }

    /// Lets viewer `i` go with its content moving at `velocity` (layout
    /// space): a pull past a limit or the bounds springs back, else each
    /// axis fast enough coasts on.
    fn release_viewer(&mut self, i: usize, content: NodeId, focus: Vec2, velocity: Vec2) {
        let v = self.viewers.list[i];
        let limits = v.limits;
        if v.scale < limits.min_scale * 0.999 || v.scale > limits.max_scale * 1.001 {
            self.glide_viewer(i, content, v.scale, focus, None);
            return;
        }
        let physics = self.scroll_physics();
        for (k, axis) in AXES.into_iter().enumerate() {
            let (lo, hi) = self.viewer_span(i, content, axis, v.scale);
            let at = v.pan.on(axis);
            let speed = velocity.on(axis);
            self.viewers.list[i].runs[k] = if at < lo || at > hi {
                Run::Spring(0.0)
            } else if speed.abs() >= physics.min_velocity {
                let speed = speed.clamp(-physics.max_velocity, physics.max_velocity);
                Run::Coast(Motion::new(f64::from(speed), physics.curve))
            } else {
                Run::Still
            };
        }
    }

    /// After a sample of pointer `pointer`'s drag on node `node` was
    /// dispatched: a viewer's drag pans it, and its end lets it go.
    pub(crate) fn steer_viewer_drag(&mut self, node: NodeId, pointer: PointerId, s: DragSample) {
        let Some(i) = self.viewer_index(node) else {
            return;
        };
        let Some(content) = self.viewer_content(node) else {
            return;
        };
        let elastic = self.contacts().is_direct(pointer);
        self.viewers.list[i].halt();
        let by = self.viewer_motion(node, s.at, s.delta);
        self.pan_viewer(i, content, by, elastic);
        if s.phase == DragPhase::End {
            let velocity = self.viewer_motion(node, s.at, s.velocity);
            let focus = self.pan_point(node, content, s.at);
            self.release_viewer(i, content, focus, velocity);
        }
        self.show_viewer(i);
    }

    /// After a sample of a pinch on node `node` was dispatched: a viewer's
    /// pinch zooms it about its middle and pans it as the middle travels,
    /// one from `touch`-like fingers pulling past its limits, and its end
    /// lets it go.
    pub(crate) fn steer_viewer_pinch(&mut self, node: NodeId, s: PinchSample, touch: bool) {
        let Some(i) = self.viewer_index(node) else {
            return;
        };
        let Some(content) = self.viewer_content(node) else {
            return;
        };
        let now = self.input_time();
        let v = &mut self.viewers.list[i];
        v.halt();
        if s.phase == PinchPhase::Start {
            v.track = VelocityTracker::default();
        }
        if let Some(now) = now {
            v.track.push(now, [s.focal.x, s.focal.y]);
        }
        let focus = self.pan_point(node, content, s.focal);
        if s.phase == PinchPhase::End {
            let velocity = if touch {
                let v = self.viewers.list[i].track.velocity();
                self.viewer_motion(node, s.focal, v)
            } else {
                Vec2::ZERO
            };
            self.release_viewer(i, content, focus, velocity);
        } else {
            if touch {
                let by = self.viewer_motion(node, s.focal, s.pan);
                self.pan_viewer(i, content, by, true);
            }
            self.zoom_viewer(i, content, s.scale_delta, focus, touch);
        }
        self.show_viewer(i);
    }

    /// Whether a touch drag of node `node` that travelled `total` from its
    /// press leaves the contact to a viewport above: `node` is a viewer whose
    /// content cannot follow along the way the drag set out, and a viewport
    /// above can.
    pub(crate) fn viewer_yields(&self, node: NodeId, total: Vec2) -> bool {
        let Some(i) = self.viewer_index(node) else {
            return false;
        };
        let Some(content) = self.viewer_content(node) else {
            return false;
        };
        let axis = if total.x.abs() > total.y.abs() {
            Axis::Row
        } else {
            Axis::Column
        };
        let v = &self.viewers.list[i];
        let (lo, hi) = self.viewer_span(i, content, axis, v.scale);
        let (at, d) = (v.pan.on(axis), total.on(axis));
        let room = if d > 0.0 { at < hi } else { at > lo };
        if room {
            return false;
        }
        self.arena()
            .parent(node)
            .is_some_and(|p| crate::input::can_scroll(self, p, axis, -d))
    }

    /// A scroll of `delta` (the offset's way) reaching viewer `node`: the
    /// content moves against it along each axis it has room on, or springs
    /// on where it springs back; a trackpad's fingers (`elastic`) pull it
    /// on past the bounds it shows pulled. Returns what of `delta` it took.
    pub(crate) fn scroll_viewer(&mut self, node: NodeId, delta: Vec2, elastic: bool) -> Vec2 {
        let Some(i) = self.viewer_index(node) else {
            return Vec2::ZERO;
        };
        let Some(content) = self.viewer_content(node) else {
            return Vec2::ZERO;
        };
        let mut took = Vec2::ZERO;
        let mut by = Vec2::ZERO;
        for (k, axis) in AXES.into_iter().enumerate() {
            let d = -delta.on(axis);
            if d == 0.0 {
                continue;
            }
            let v = &self.viewers.list[i];
            let (lo, hi) = self.viewer_span(i, content, axis, v.scale);
            let at = v.pan.on(axis);
            let springing = matches!(v.runs[k], Run::Spring(_));
            let outside = at < lo || at > hi;
            let room = if d > 0.0 { at < hi } else { at > lo };
            if springing && !elastic {
                // The system's coast leaves a bounce be.
                *on(&mut took, axis) = -d;
                continue;
            }
            if room || (elastic && outside) {
                *on(&mut took, axis) = -d;
                *on(&mut by, axis) = d;
            }
        }
        if took != Vec2::ZERO {
            let v = &mut self.viewers.list[i];
            v.glide = None;
            for (k, axis) in AXES.into_iter().enumerate() {
                if by.on(axis) != 0.0 {
                    v.runs[k] = Run::Still;
                }
            }
            self.pan_viewer(i, content, by, elastic);
            self.show_viewer(i);
        }
        took
    }

    /// The trackpad's fingers lifted from over `target` moving the offsets
    /// at `velocity`: the viewer they moved lets go, coasting at that speed
    /// unless the system coasts on, and the rest of `velocity` is returned
    /// for the viewports above.
    pub(crate) fn lift_viewer(&mut self, target: NodeId, velocity: Vec2) -> Vec2 {
        let Some(i) = self.viewer_over(target) else {
            return velocity;
        };
        let node = self.viewers.list[i].node;
        let Some(content) = self.viewer_content(node) else {
            return velocity;
        };
        let mut rest = velocity;
        let mut moving = Vec2::ZERO;
        for axis in AXES {
            let d = -velocity.on(axis);
            let v = &self.viewers.list[i];
            let (lo, hi) = self.viewer_span(i, content, axis, v.scale);
            let at = v.pan.on(axis);
            let room = if d > 0.0 { at < hi } else { at > lo };
            if d == 0.0 || room || at < lo || at > hi {
                *on(&mut rest, axis) = 0.0;
                *on(&mut moving, axis) = d;
            }
        }
        if self.scroll_physics().system_coast {
            moving = Vec2::ZERO;
        }
        let focus = self.viewer_middle(node, content);
        self.release_viewer(i, content, focus, moving);
        rest
    }

    /// A press at or under `target`: the coasts of the viewers enclosing it
    /// stop. Returns whether one moved briskly, so the press is the catch.
    pub(crate) fn catch_viewers(&mut self, target: NodeId) -> bool {
        let mut brisk = false;
        for i in 0..self.viewers.list.len() {
            let node = self.viewers.list[i].node;
            if !self.arena().is_live(node) || !self.within(target, node) {
                continue;
            }
            for run in &mut self.viewers.list[i].runs {
                if let Run::Coast(m) = *run {
                    brisk |= m.velocity.abs() > CATCH_VELOCITY;
                    *run = Run::Still;
                }
            }
        }
        brisk
    }

    /// Stops the coasts of the viewers at or above `target`: a wheel there
    /// moves them now. A spring back springs on.
    pub(crate) fn stop_viewer_coasts(&mut self, target: NodeId) {
        for i in 0..self.viewers.list.len() {
            let node = self.viewers.list[i].node;
            if !self.arena().is_live(node) || !self.within(target, node) {
                continue;
            }
            for run in &mut self.viewers.list[i].runs {
                if matches!(run, Run::Coast(_)) {
                    *run = Run::Still;
                }
            }
        }
    }

    /// Advances every viewer's glide, coasts and springs by a frame of
    /// `delta`: a coast reaching the bounds bounces where viewports bounce
    /// and stops there elsewhere, a spring that arrived rests on the bound
    /// and a glide that arrived ends.
    pub(crate) fn tick_viewers(&mut self, delta: Duration) {
        let dt = delta.as_secs_f64().min(MAX_STEP);
        let mut i = self.viewers.list.len();
        while i > 0 {
            i -= 1;
            let node = self.viewers.list[i].node;
            if !self.arena().is_live(node) {
                self.viewers.list.swap_remove(i);
                continue;
            }
            if !self.viewers.list[i].moving() {
                continue;
            }
            let Some(content) = self.viewer_content(node) else {
                self.viewers.list[i].halt();
                continue;
            };
            let mut v = self.viewers.list[i];
            if let Some(mut g) = v.glide {
                g.t += dt;
                let left = (1.0 + GLIDE * g.t) * (-GLIDE * g.t).exp();
                let p = if left < 1e-3 {
                    1.0
                } else {
                    (1.0 - left) as f32
                };
                v.scale = (g.s0.ln() + (g.s1.ln() - g.s0.ln()) * p).exp();
                v.pan = g.p0 + (g.p1 - g.p0) * p - g.q * v.scale;
                if p >= 1.0 {
                    (v.scale, v.pan) = g.end();
                    v.glide = None;
                } else {
                    v.glide = Some(g);
                }
            }
            self.viewers.list[i] = v;
            for (k, axis) in AXES.into_iter().enumerate() {
                let (lo, hi) = self.viewer_span(i, content, axis, v.scale);
                let pull = f64::from(self.viewer_pull(i, axis));
                let at = v.pan.on(axis);
                let (next, run) = match v.runs[k] {
                    Run::Still => continue,
                    Run::Coast(mut m) => {
                        let wanted = at + m.advance(delta) as f32;
                        if wanted < lo || wanted > hi {
                            let edge = wanted.clamp(lo, hi);
                            if pull > 0.0 && !m.done() && hi > lo {
                                (edge, Run::Spring(m.velocity))
                            } else {
                                (edge, Run::Still)
                            }
                        } else if m.done() {
                            (wanted, Run::Still)
                        } else {
                            (wanted, Run::Coast(m))
                        }
                    }
                    Run::Spring(speed) => {
                        // A critically damped spring about the nearest
                        // bound: e(t) = (e + (v + λe)t)·e^(−λt).
                        let target = at.clamp(lo, hi);
                        let e = f64::from(at - target);
                        let c = speed + SPRING * e;
                        let decay = (-SPRING * dt).exp();
                        let e = ((e + c * dt) * decay).clamp(-pull, pull);
                        let speed = (speed - SPRING * c * dt) * decay;
                        if e.abs() < SPRING_NEAR && speed.abs() < SPRING_STILL {
                            (target, Run::Still)
                        } else {
                            (target + e as f32, Run::Spring(speed))
                        }
                    }
                };
                *on(&mut v.pan, axis) = next;
                v.runs[k] = run;
            }
            self.viewers.list[i] = v;
            self.show_viewer(i);
        }
    }

    /// Once layout placed the tree: each viewer at rest keeps its content
    /// within the bounds and limits the new boxes set, and every one shows
    /// its content where it stands.
    pub(crate) fn fit_viewers(&mut self) {
        if self.viewers.list.is_empty() {
            return;
        }
        let mut list = std::mem::take(&mut self.viewers.list);
        list.retain(|v| self.arena().is_live(v.node));
        self.viewers.list = list;
        for i in 0..self.viewers.list.len() {
            let v = self.viewers.list[i];
            let Some(content) = self.viewer_content(v.node) else {
                continue;
            };
            if !v.moving() && !self.dragging(v.node) && !self.pinching(v.node) {
                let v = &mut self.viewers.list[i];
                v.scale = v.scale.clamp(v.limits.min_scale, v.limits.max_scale);
                self.clamp_viewer(i, content);
            }
            self.show_viewer(i);
        }
    }

    /// Key `ev` on the viewer the keyboard is in under `root`: arrows and
    /// page keys pan it while it has room that way, `+` and `-` zoom it
    /// about its middle, `0` and Home put it back. Returns whether it took
    /// the key.
    pub(crate) fn viewer_key(&mut self, root: NodeId, ev: &KeyEvent) -> bool {
        if !ev.pressed || self.viewers.list.is_empty() {
            return false;
        }
        let from = self
            .focused()
            .or_else(|| self.hovered())
            .filter(|&n| self.arena().is_live(n) && self.within(n, root));
        let Some(i) = from.and_then(|n| self.viewer_over(n)) else {
            return false;
        };
        let node = self.viewers.list[i].node;
        let Some(content) = self.viewer_content(node) else {
            return false;
        };
        let m = ev.modifiers;
        let plain = !m.control && !m.alt && !m.logo;
        if !plain {
            return false;
        }
        let page = self.bounds(node).h * PAGE_SHARE;
        let by = match ev.key {
            Key::Equal => {
                return self.key_move(
                    node,
                    ViewerMove::ZoomBy {
                        factor: KEY_ZOOM,
                        at: None,
                    },
                );
            }
            Key::Minus if !m.shift => {
                return self.key_move(
                    node,
                    ViewerMove::ZoomBy {
                        factor: 1.0 / KEY_ZOOM,
                        at: None,
                    },
                );
            }
            Key::Digit0 | Key::Home if !m.shift => return self.key_move(node, ViewerMove::Reset),
            Key::Left if !m.shift => Vec2::new(LINE_STEP, 0.0),
            Key::Right if !m.shift => Vec2::new(-LINE_STEP, 0.0),
            Key::Up if !m.shift => Vec2::new(0.0, LINE_STEP),
            Key::Down if !m.shift => Vec2::new(0.0, -LINE_STEP),
            Key::PageUp if !m.shift => Vec2::new(0.0, page),
            Key::PageDown if !m.shift => Vec2::new(0.0, -page),
            _ => return false,
        };
        let (scale, pan) = self.viewers.list[i].aim();
        let room = AXES.into_iter().any(|axis| {
            let d = by.on(axis);
            let (lo, hi) = self.viewer_span(i, content, axis, scale);
            let at = pan.on(axis);
            (d > 0.0 && at < hi) || (d < 0.0 && at > lo)
        });
        room && self.key_move(node, ViewerMove::Pan(by))
    }

    fn key_move(&mut self, node: NodeId, to: ViewerMove) -> bool {
        self.move_viewer(node, to);
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::component::{BuildCx, LeafStyle, StackStyle};
    use crate::layout::Size;
    use crate::style::BoxStyle;
    use viso_render::Rect;

    const FRAME: Duration = Duration::from_millis(16);

    /// A 200×100 viewer holding content laid out to fill it.
    fn viewer(limits: ViewerLimits) -> (NodeStore, NodeId, NodeId) {
        let mut store = NodeStore::new();
        let mut content = None;
        let root = {
            let mut cx = BuildCx::new(&mut store);
            let root = cx.stack(
                StackStyle {
                    align: Alignment2D::Center,
                    size: Size::fixed(200.0, 100.0),
                    ..StackStyle::default()
                },
                |cx| {
                    content = Some(
                        cx.leaf(LeafStyle {
                            size: Size::fill(),
                            style: BoxStyle::NONE,
                        })
                        .id(),
                    );
                },
            );
            cx.clip(root, true);
            root.id()
        };
        store.set_viewer(root, Some(limits));
        store.layout(root, Rect::new(0.0, 0.0, 200.0, 100.0), &mut Vec::new());
        (store, root, content.unwrap())
    }

    fn settle(store: &mut NodeStore) {
        for _ in 0..200 {
            if !store.viewers.moving() {
                return;
            }
            store.tick_viewers(FRAME);
        }
        panic!("the viewer never came to rest");
    }

    #[test]
    fn a_zoom_about_a_point_keeps_it_in_place_and_shows_through_the_transform() {
        let (mut store, root, content) = viewer(ViewerLimits::default());
        store.move_viewer(
            root,
            ViewerMove::Zoom {
                scale: 2.0,
                at: Some(Vec2::new(50.0, 25.0)),
            },
        );
        assert!(store.viewers.moving(), "it glides");
        settle(&mut store);
        assert_eq!(store.viewer_scale(root), Some(2.0));
        assert_eq!(store.viewer_pan(root), Some(Vec2::new(-50.0, -25.0)));
        assert_eq!(store.translate(content), Vec2::new(-50.0, -25.0));
        assert_eq!(store.turn(content).scale, 2.0);
        // The point zoomed about shows where it was.
        let world = store.world_transform(content);
        let p = world.transform_point(Vec2::new(50.0, 25.0));
        assert!((p - Vec2::new(50.0, 25.0)).length() < 1e-3, "{p:?}");
        let q = world.transform_point(Vec2::new(60.0, 25.0));
        assert!((q - Vec2::new(70.0, 25.0)).length() < 1e-3, "{q:?}");
    }

    #[test]
    fn the_content_keeps_its_edges_past_the_viewer_s_and_stays_in_the_middle_when_smaller() {
        let (mut store, root, _) = viewer(ViewerLimits {
            min_scale: 0.5,
            ..ViewerLimits::default()
        });
        store.set_reduced_motion(true);
        store.move_viewer(
            root,
            ViewerMove::Zoom {
                scale: 2.0,
                at: None,
            },
        );
        store.move_viewer(root, ViewerMove::Pan(Vec2::new(500.0, -500.0)));
        assert_eq!(store.viewer_pan(root), Some(Vec2::new(0.0, -100.0)));
        store.move_viewer(
            root,
            ViewerMove::Zoom {
                scale: 0.5,
                at: None,
            },
        );
        assert_eq!(store.viewer_pan(root), Some(Vec2::new(50.0, 25.0)));
        store.move_viewer(root, ViewerMove::Pan(Vec2::new(30.0, 0.0)));
        assert_eq!(store.viewer_pan(root), Some(Vec2::new(50.0, 25.0)));
        store.move_viewer(
            root,
            ViewerMove::Zoom {
                scale: 9.0,
                at: None,
            },
        );
        assert_eq!(store.viewer_scale(root), Some(4.0), "within the limits");
    }

    #[test]
    fn a_margin_lets_the_content_move_that_much_further() {
        let (mut store, root, _) = viewer(ViewerLimits {
            margin: 20.0,
            ..ViewerLimits::default()
        });
        store.set_reduced_motion(true);
        store.move_viewer(root, ViewerMove::Pan(Vec2::new(-100.0, 100.0)));
        assert_eq!(store.viewer_pan(root), Some(Vec2::new(-20.0, 20.0)));
    }

    #[test]
    fn a_coast_slows_to_a_stop_and_a_pull_past_the_bounds_springs_back() {
        let (mut store, root, content) = viewer(ViewerLimits::default());
        store.set_reduced_motion(true);
        store.move_viewer(
            root,
            ViewerMove::Zoom {
                scale: 4.0,
                at: None,
            },
        );
        store.set_reduced_motion(false);
        let i = store.viewer_index(root).unwrap();
        let focus = store.viewer_middle(root, content);
        store.release_viewer(i, content, focus, Vec2::new(-300.0, 0.0));
        let start = store.viewer_pan(root).unwrap();
        settle(&mut store);
        let end = store.viewer_pan(root).unwrap();
        assert!(end.x < start.x - 20.0, "it coasts the way it moved");
        assert_eq!(end.y, start.y);

        store.set_scroll_physics(crate::ScrollPhysics::APPLE);
        store.pan_viewer(i, content, Vec2::new(0.0, 500.0), true);
        let pulled = store.viewer_pan(root).unwrap().y;
        assert!(
            pulled > 0.0 && pulled < 35.0,
            "a pull shows eased: {pulled}"
        );
        store.release_viewer(i, content, focus, Vec2::ZERO);
        settle(&mut store);
        assert_eq!(store.viewer_pan(root).unwrap().y, 0.0, "and springs back");
    }

    #[test]
    fn a_pinch_past_the_limits_glides_back_about_its_middle() {
        let (mut store, root, content) = viewer(ViewerLimits::default());
        store.set_scroll_physics(crate::ScrollPhysics::APPLE);
        let sample = |phase, scale_delta| PinchSample {
            phase,
            focal: Vec2::new(100.0, 50.0),
            scale: 1.0,
            rotation: 0.0,
            scale_delta,
            rotation_delta: 0.0,
            pan: Vec2::ZERO,
        };
        store.steer_viewer_pinch(root, sample(PinchPhase::Start, 0.5), true);
        let shown = store.viewer_scale(root).unwrap();
        assert!(
            shown < 1.0 && shown > 0.5,
            "a pinch past the limit shows eased"
        );
        store.steer_viewer_pinch(root, sample(PinchPhase::End, 1.0), true);
        settle(&mut store);
        assert_eq!(store.viewer_scale(root), Some(1.0));
        assert_eq!(store.viewer_pan(root), Some(Vec2::ZERO));
        let _ = content;
    }

    /// Drives the router over a store: the clock steps 10 ms a sample.
    struct Rig {
        store: NodeStore,
        states: crate::state::StateStore,
        bindings: crate::binding::BindingTable,
        root: NodeId,
        chain: Vec<NodeId>,
        clock: viso_runtime::Instant,
    }

    impl Rig {
        fn new(store: NodeStore, root: NodeId) -> Rig {
            Rig {
                store,
                states: crate::state::StateStore::new(),
                bindings: crate::binding::BindingTable::new(),
                root,
                chain: Vec::new(),
                clock: viso_runtime::Instant::now(),
            }
        }

        fn tick(&mut self) {
            self.clock += Duration::from_millis(10);
            self.store.set_input_time(self.clock);
        }

        fn pointer(
            &mut self,
            id: u64,
            direct: bool,
            x: f32,
            y: f32,
            phase: crate::input::PointerPhase,
        ) {
            use crate::input::{PointerButtons, PointerContact, PointerEvent, PointerPhase};
            self.tick();
            let buttons = if phase == PointerPhase::Up {
                PointerButtons::NONE
            } else {
                PointerButtons::PRIMARY
            };
            crate::input::route_contact(
                &mut self.store,
                &mut self.states,
                &self.bindings,
                self.root,
                PointerContact {
                    id: PointerId(id),
                    direct,
                },
                PointerEvent {
                    x,
                    y,
                    phase,
                    buttons,
                    modifiers: crate::input::Modifiers::default(),
                },
                &mut self.chain,
            );
        }

        /// A press at `from` dragged through `to` and lifted there.
        fn drag(&mut self, direct: bool, from: [f32; 2], to: &[[f32; 2]]) {
            use crate::input::PointerPhase;
            let id = if direct { 1 } else { 0 };
            self.pointer(id, direct, from[0], from[1], PointerPhase::Down);
            for at in to {
                self.pointer(id, direct, at[0], at[1], PointerPhase::Move);
            }
            let last = to.last().copied().unwrap_or(from);
            self.pointer(id, direct, last[0], last[1], PointerPhase::Up);
        }

        fn wheel(&mut self, x: f32, y: f32, delta: Vec2) {
            self.tick();
            crate::input::route_scroll(
                &mut self.store,
                self.root,
                crate::input::ScrollEvent {
                    x,
                    y,
                    delta_x: delta.x,
                    delta_y: delta.y,
                    modifiers: crate::input::Modifiers::default(),
                    phase: crate::input::ScrollPhase::None,
                },
            );
        }
    }

    #[test]
    fn a_mouse_drag_pans_the_zoomed_content_and_stops_at_its_bounds() {
        let (mut store, root, _) = viewer(ViewerLimits::default());
        store.set_reduced_motion(true);
        store.move_viewer(
            root,
            ViewerMove::Zoom {
                scale: 2.0,
                at: Some(Vec2::new(0.0, 0.0)),
            },
        );
        let mut rig = Rig::new(store, root);
        rig.drag(false, [100.0, 50.0], &[[80.0, 40.0], [60.0, 30.0]]);
        assert_eq!(rig.store.viewer_pan(root), Some(Vec2::new(-40.0, -20.0)));
        settle(&mut rig.store);
        rig.drag(false, [100.0, 50.0], &[[-400.0, -400.0]]);
        settle(&mut rig.store);
        assert_eq!(
            rig.store.viewer_pan(root),
            Some(Vec2::new(-200.0, -100.0)),
            "a mouse pulls nothing past"
        );
    }

    #[test]
    fn a_flick_coasts_on_and_a_wheel_pans() {
        let (mut store, root, _) = viewer(ViewerLimits::default());
        store.set_reduced_motion(true);
        store.move_viewer(
            root,
            ViewerMove::Zoom {
                scale: 4.0,
                at: None,
            },
        );
        store.set_reduced_motion(false);
        let start = store.viewer_pan(root).unwrap();
        let mut rig = Rig::new(store, root);
        rig.drag(
            true,
            [100.0, 50.0],
            &[[90.0, 50.0], [70.0, 50.0], [50.0, 50.0], [30.0, 50.0]],
        );
        let lifted = rig.store.viewer_pan(root).unwrap();
        assert_eq!(lifted.x, start.x - 70.0);
        assert!(rig.store.is_scroll_moving(), "it coasts");
        settle(&mut rig.store);
        assert!(rig.store.viewer_pan(root).unwrap().x < lifted.x - 50.0);

        let before = rig.store.viewer_pan(root).unwrap();
        rig.wheel(100.0, 50.0, Vec2::new(0.0, 30.0));
        assert_eq!(
            rig.store.viewer_pan(root).unwrap(),
            before - Vec2::new(0.0, 30.0)
        );
    }

    #[test]
    fn a_control_wheel_zooms_about_the_pointer() {
        let (store, root, _) = viewer(ViewerLimits::default());
        let mut rig = Rig::new(store, root);
        let Rig {
            store,
            states,
            bindings,
            chain,
            ..
        } = &mut rig;
        crate::pinch::route_wheel_zoom(store, states, bindings, root, (40.0, 20.0, -100.0), chain);
        let scale = store.viewer_scale(root).unwrap();
        assert!((scale - core::f32::consts::E).abs() < 1e-3, "{scale}");
        let pan = store.viewer_pan(root).unwrap();
        assert!((pan - Vec2::new(40.0, 20.0) * (1.0 - scale)).length() < 1e-3);
        crate::pinch::route_wheel_zoom(store, states, bindings, root, (40.0, 20.0, -200.0), chain);
        assert_eq!(
            store.viewer_scale(root),
            Some(4.0),
            "no further than its limit"
        );
    }

    #[test]
    fn a_touch_drag_the_content_cannot_follow_is_left_to_the_viewport_above() {
        use crate::component::{FlexStyle, ScrollStyle};
        let mut store = NodeStore::new();
        let mut viewers = Vec::new();
        let root = {
            let mut cx = BuildCx::new(&mut store);
            cx.scroll(
                ScrollStyle {
                    axis: Axis::Row,
                    size: Size::fixed(200.0, 100.0),
                    ..ScrollStyle::default()
                },
                |cx| {
                    cx.flex(FlexStyle::default(), |cx| {
                        for _ in 0..2 {
                            let v = cx.stack(
                                StackStyle {
                                    size: Size::fixed(200.0, 100.0),
                                    ..StackStyle::default()
                                },
                                |cx| {
                                    cx.leaf(LeafStyle {
                                        size: Size::fill(),
                                        style: BoxStyle::NONE,
                                    });
                                },
                            );
                            viewers.push(v.id());
                        }
                    });
                },
            )
            .id()
        };
        for &v in &viewers {
            store.set_viewer(v, Some(ViewerLimits::default()));
        }
        store.layout(root, Rect::new(0.0, 0.0, 200.0, 100.0), &mut Vec::new());
        let mut rig = Rig::new(store, root);
        rig.drag(true, [150.0, 50.0], &[[140.0, 50.0], [100.0, 50.0]]);
        assert_eq!(rig.store.scroll(root).x, 50.0, "the row pages");
        assert_eq!(rig.store.viewer_pan(viewers[0]), Some(Vec2::ZERO));

        rig.store.set_reduced_motion(true);
        rig.store.set_scroll(root, Vec2::ZERO);
        rig.store.move_viewer(
            viewers[0],
            ViewerMove::Zoom {
                scale: 2.0,
                at: Some(Vec2::new(0.0, 50.0)),
            },
        );
        rig.drag(true, [150.0, 50.0], &[[140.0, 50.0], [100.0, 50.0]]);
        assert_eq!(rig.store.scroll(root).x, 0.0, "a zoomed photo pans first");
        assert_eq!(rig.store.viewer_pan(viewers[0]).unwrap().x, -50.0);
    }

    #[test]
    fn keys_pan_zoom_and_put_back_the_viewer_the_keyboard_is_in() {
        use crate::input::{KeyEvent, Modifiers};
        let (mut store, root, _) = viewer(ViewerLimits::default());
        store.set_reduced_motion(true);
        store.set_focusable(root, true);
        store.set_focused(Some(root));
        let key = |store: &mut NodeStore, key| {
            store.scroll_by_key(
                root,
                &KeyEvent {
                    key,
                    pressed: true,
                    repeat: false,
                    modifiers: Modifiers::default(),
                },
                false,
            )
        };
        assert!(!key(&mut store, Key::Right), "nothing to pan at rest");
        assert!(key(&mut store, Key::Equal));
        assert_eq!(store.viewer_scale(root), Some(KEY_ZOOM));
        assert!(key(&mut store, Key::Right));
        assert_eq!(store.viewer_pan(root).unwrap().x, -50.0, "to the bound");
        assert!(key(&mut store, Key::Digit0));
        assert_eq!(store.viewer_scale(root), Some(1.0));
        assert_eq!(store.viewer_pan(root), Some(Vec2::ZERO));
    }
}
