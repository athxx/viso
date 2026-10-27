//! Progress indicators: a node's drawing of how far work has come.
//!
//! A progress indicator is a node's content ([`Content::Progress`]): a bar
//! filled from its leading edge, or a ring drawn round clockwise from the
//! top, over a faint track. Told how far the work has come it eases to the
//! new fraction, or lands on it at once under reduced motion; told nothing,
//! work of unknown length runs, a stretch of fill sweeping along the bar or
//! an arc chasing round the ring, repainted each frame
//! ([`NodeStore::tick_progress`]).

use std::f32::consts::TAU;
use std::time::Duration;

use viso_render::{AnalyticCapsule, Border, LineCap, Path, Point, Primitive, Rect, Rgba, Stroke};

use crate::component::{BuildCx, Handle, LeafStyle, NodeStore};
use crate::content::Content;
use crate::dirty::DirtyClass;
use crate::layout::{Sizing, Vec2};
use crate::node::NodeId;
use crate::spinner::{arc, chase, fast_out_slow_in};
use crate::style::BoxStyle;

/// How a progress indicator draws.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProgressShape {
    /// A track filled from its leading edge.
    Bar,
    /// A ring drawn round clockwise from the top.
    Ring,
}

/// What a progress indicator node draws.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Progress {
    pub shape: ProgressShape,
    /// How far the work has come, `0` to `1`; `None` while work of unknown
    /// length runs.
    pub value: Option<f32>,
    /// The fraction drawn, easing toward `value`.
    pub shown: f32,
    /// The fill's color; the track is it, faint.
    pub color: Rgba,
    /// How long work of unknown length has run (seconds).
    pub time: f32,
    /// The size it measures to.
    pub natural: Vec2,
}

/// How faint the track is against the fill.
const TRACK: f32 = 0.24;
/// How quickly the fill eases to a new value: the time constant (seconds).
const EASE: f32 = 0.08;
/// How long a bar's sweep runs.
const SWEEP: f32 = 1.8;

impl Progress {
    /// An indicator of `shape`, `natural` in size, inked `color`, showing
    /// `value`.
    pub fn new(shape: ProgressShape, natural: Vec2, color: Rgba, value: Option<f32>) -> Progress {
        let value = value.map(|v| v.clamp(0.0, 1.0));
        Progress {
            shape,
            value,
            shown: value.unwrap_or(0.0),
            color,
            time: 0.0,
            natural,
        }
    }

    /// Whether it moves: it runs, or eases toward its value.
    fn moving(&self) -> bool {
        self.value.is_none_or(|v| v != self.shown)
    }

    /// Draws it into `rect` (world space), a bar filling from the right
    /// when `rtl`.
    pub(crate) fn paint(&self, rect: Rect, rtl: bool, out: &mut Vec<Primitive>) {
        if rect.w <= 0.0 || rect.h <= 0.0 || self.color.a <= 0.0 {
            return;
        }
        match self.shape {
            ProgressShape::Bar => self.paint_bar(rect, rtl, out),
            ProgressShape::Ring => self.paint_ring(rect, out),
        }
    }

    fn paint_bar(&self, rect: Rect, rtl: bool, out: &mut Vec<Primitive>) {
        let track = faded(self.color, TRACK);
        out.push(capsule(rect, track));
        // The stretch filled, as fractions of the track from its leading edge.
        let (from, to) = match self.value {
            Some(_) => (0.0, self.shown),
            None => {
                let u = (self.time / SWEEP).fract() * 4.0 / 3.0;
                let head = fast_out_slow_in(u.min(1.0));
                let tail = fast_out_slow_in((u - 1.0 / 3.0).clamp(0.0, 1.0));
                (tail, head)
            }
        };
        if to - from <= 0.0 {
            return;
        }
        let (x0, x1) = if rtl {
            (rect.w * (1.0 - to), rect.w * (1.0 - from))
        } else {
            (rect.w * from, rect.w * to)
        };
        let fill = Rect {
            x: rect.x + x0,
            w: x1 - x0,
            ..rect
        };
        out.push(capsule(fill, self.color));
    }

    fn paint_ring(&self, rect: Rect, out: &mut Vec<Primitive>) {
        let side = rect.w.min(rect.h);
        let width = (side * 0.1).max(1.0);
        let radius = side * 0.5 - width * 0.5;
        let center = Point {
            x: rect.x + rect.w * 0.5,
            y: rect.y + rect.h * 0.5,
        };
        let stroke = |cmds, color| {
            let mut stroke = Stroke::new(width, color);
            stroke.cap = LineCap::Round;
            Primitive::Path(Box::new(Path {
                cmds,
                fill: None,
                stroke: Some(stroke),
                shadow: None,
            }))
        };
        // Turns clockwise from the top.
        let (from, to) = match self.value {
            Some(_) => {
                let mut track = Vec::with_capacity(6);
                arc(&mut track, center, radius, 0.0, TAU);
                out.push(stroke(track, faded(self.color, TRACK)));
                (0.0, self.shown)
            }
            None => chase(self.time, width / (TAU * radius)),
        };
        if to - from <= 0.0 {
            return;
        }
        let mut cmds = Vec::with_capacity(14);
        arc(&mut cmds, center, radius, from * TAU, to * TAU);
        out.push(stroke(cmds, self.color));
    }
}

/// A capsule filling `rect` in `color`.
fn capsule(rect: Rect, color: Rgba) -> Primitive {
    Primitive::AnalyticCapsule(AnalyticCapsule {
        rect,
        color,
        border: Border::NONE,
    })
}

/// `color` at `alpha` of its opacity.
fn faded(color: Rgba, alpha: f32) -> Rgba {
    Rgba {
        a: color.a * alpha,
        ..color
    }
}

/// The progress indicators of a store that move: those that run, and those
/// easing to a new value.
#[derive(Debug, Default)]
pub(crate) struct Progresses {
    moving: Vec<NodeId>,
}

impl Progresses {
    pub(crate) fn clear(&mut self) {
        self.moving.clear();
    }

    /// Forgets the indicator of the slot `index`, freed or reused.
    pub(crate) fn forget(&mut self, index: u32) {
        self.moving.retain(|id| id.index() != index);
    }
}

impl BuildCx<'_> {
    /// A progress indicator leaf of `shape`, asking `size` of its parent and
    /// measuring to `natural`, inked `color`, showing `value`; see
    /// [`NodeStore::set_progress`].
    pub fn progress(
        &mut self,
        shape: ProgressShape,
        size: Sizing,
        natural: Vec2,
        color: Rgba,
        value: Option<f32>,
    ) -> Handle {
        let handle = self.leaf(LeafStyle {
            size,
            style: BoxStyle::NONE,
        });
        self.store
            .set_progress(handle.id(), shape, natural, color, value);
        handle
    }
}

impl NodeStore {
    /// Node `id`'s progress indicator, if it draws one.
    pub fn progress(&self, id: NodeId) -> Option<Progress> {
        match self.content_payload(id) {
            Some(Content::Progress(progress)) => Some(*progress),
            _ => None,
        }
    }

    /// Make node `id` a progress indicator of `shape`, `natural` in size,
    /// inked `color`, showing `value` (`0` to `1`, `None` while work of
    /// unknown length runs). One already is keeps what it shows and eases
    /// from there.
    pub fn set_progress(
        &mut self,
        id: NodeId,
        shape: ProgressShape,
        natural: Vec2,
        color: Rgba,
        value: Option<f32>,
    ) {
        if !self.arena().is_live(id) {
            return;
        }
        let mut color = color;
        self.theme_part(id, [&mut color]);
        let value = value.map(|v| if v.is_nan() { 0.0 } else { v.clamp(0.0, 1.0) });
        let snap = self.reduced_motion();
        let next = match self.progress(id) {
            Some(was) if was.shape == shape => {
                let shown = match (was.value, value) {
                    // Run in, the fill grows from nothing.
                    (None, Some(_)) => 0.0,
                    _ => was.shown,
                };
                let mut next = Progress {
                    value,
                    shown,
                    color,
                    natural,
                    time: if value.is_some() { 0.0 } else { was.time },
                    ..was
                };
                if snap {
                    next.shown = value.unwrap_or(next.shown);
                }
                if next == was {
                    return;
                }
                if next.natural != was.natural {
                    self.mark_dirty(id, DirtyClass::MEASURE);
                }
                next
            }
            _ => {
                self.mark_dirty(id, DirtyClass::MEASURE);
                Progress::new(shape, natural, color, value)
            }
        };
        let moving = next.moving();
        self.set_content_payload(id, Content::Progress(next));
        self.mark_dirty(id, DirtyClass::PAINT);
        let list = &mut self.progresses.moving;
        match (moving, list.contains(&id)) {
            (true, false) => list.push(id),
            (false, true) => list.retain(|&m| m != id),
            _ => {}
        }
    }

    /// Whether a progress indicator that shows moves: the frame loop beats
    /// while one does.
    #[inline]
    pub fn is_progressing(&self) -> bool {
        self.progresses.moving.iter().any(|&id| self.shows(id))
    }

    /// Advance every moving progress indicator by a frame of `delta`,
    /// repainting it; one that lands on its value stops.
    pub fn tick_progress(&mut self, delta: Duration) {
        let dt = delta.as_secs_f32().min(0.1);
        let mut i = self.progresses.moving.len();
        while i > 0 {
            i -= 1;
            let id = self.progresses.moving[i];
            // One out of sight waits where it is until it shows again.
            if !self.shows(id) {
                if self.progress(id).is_none() {
                    self.progresses.moving.swap_remove(i);
                }
                continue;
            }
            let Some(progress) = self.progress_mut(id) else {
                self.progresses.moving.swap_remove(i);
                continue;
            };
            match progress.value {
                None => progress.time += dt,
                Some(value) => {
                    let gap = value - progress.shown;
                    progress.shown = if gap.abs() < 5e-4 {
                        value
                    } else {
                        progress.shown + gap * (1.0 - (-dt / EASE).exp())
                    };
                }
            }
            let settled = !progress.moving();
            self.mark_dirty(id, DirtyClass::PAINT);
            if settled {
                self.progresses.moving.swap_remove(i);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const BLUE: Rgba = Rgba::new(0.2, 0.4, 0.9, 1.0);
    const BAR: Vec2 = Vec2 { x: 100.0, y: 4.0 };

    fn painted(progress: &Progress, rtl: bool) -> Vec<Rect> {
        let mut out = Vec::new();
        let rect = Rect {
            x: 10.0,
            y: 10.0,
            w: 100.0,
            h: 4.0,
        };
        progress.paint(rect, rtl, &mut out);
        out.iter()
            .map(|p| match p {
                Primitive::AnalyticCapsule(c) => c.rect,
                _ => unreachable!(),
            })
            .collect()
    }

    #[test]
    fn a_bar_fills_from_its_leading_edge_over_a_faint_track() {
        let p = Progress::new(ProgressShape::Bar, BAR, BLUE, Some(0.25));
        let ltr = painted(&p, false);
        assert_eq!(ltr.len(), 2, "the track and the fill");
        assert_eq!((ltr[1].x, ltr[1].w), (10.0, 25.0));
        let rtl = painted(&p, true);
        assert_eq!((rtl[1].x, rtl[1].w), (85.0, 25.0));
        let empty = Progress::new(ProgressShape::Bar, BAR, BLUE, Some(0.0));
        assert_eq!(painted(&empty, false).len(), 1, "the track alone");
    }

    #[test]
    fn a_bar_of_unknown_length_sweeps_a_stretch_along_it() {
        let mut p = Progress::new(ProgressShape::Bar, BAR, BLUE, None);
        let mut starts = Vec::new();
        for t in [0.3, 0.6, 0.9, 1.2] {
            p.time = t;
            let rects = painted(&p, false);
            assert_eq!(rects.len(), 2);
            let fill = rects[1];
            assert!(fill.x >= 10.0 && fill.x + fill.w <= 110.0 + 1e-3);
            starts.push(fill.x);
        }
        assert!(starts.windows(2).all(|w| w[1] >= w[0]), "{starts:?}");
    }

    #[test]
    fn a_ring_draws_its_track_and_arc_or_chases_round() {
        let mut out = Vec::new();
        let rect = Rect {
            x: 0.0,
            y: 0.0,
            w: 32.0,
            h: 32.0,
        };
        let natural = Vec2 { x: 32.0, y: 32.0 };
        Progress::new(ProgressShape::Ring, natural, BLUE, Some(0.5)).paint(rect, false, &mut out);
        assert_eq!(out.len(), 2, "the track and the arc");
        out.clear();
        Progress::new(ProgressShape::Ring, natural, BLUE, None).paint(rect, false, &mut out);
        assert_eq!(out.len(), 1, "the chasing arc alone");
    }

    #[test]
    fn a_new_value_eases_in_and_lands_at_once_under_reduced_motion() {
        let mut store = NodeStore::new();
        let mut cx = BuildCx::new(&mut store);
        let bar = cx
            .leaf(LeafStyle {
                size: Sizing::fixed(100.0, 4.0),
                style: BoxStyle::NONE,
            })
            .id();
        store.set_progress(bar, ProgressShape::Bar, BAR, BLUE, Some(0.0));
        assert!(!store.is_progressing(), "it starts where it stands");
        store.set_progress(bar, ProgressShape::Bar, BAR, BLUE, Some(1.0));
        assert!(store.is_progressing());
        store.clear_dirty();
        store.tick_progress(Duration::from_millis(16));
        let shown = store.progress(bar).unwrap().shown;
        assert!(shown > 0.0 && shown < 1.0, "{shown}");
        assert!(store.dirty(bar).contains(DirtyClass::PAINT));
        for _ in 0..120 {
            store.tick_progress(Duration::from_millis(16));
        }
        assert_eq!(store.progress(bar).unwrap().shown, 1.0);
        assert!(!store.is_progressing(), "landed, it stops");

        store.set_progress(bar, ProgressShape::Bar, BAR, BLUE, None);
        assert!(store.is_progressing(), "unknown length runs");
        store.tick_progress(Duration::from_millis(16));
        assert!(store.progress(bar).unwrap().time > 0.0);

        store.set_reduced_motion(true);
        store.set_progress(bar, ProgressShape::Bar, BAR, BLUE, Some(0.4));
        assert_eq!(store.progress(bar).unwrap().shown, 0.4);
        assert!(!store.is_progressing());
    }

    #[test]
    fn one_out_of_sight_beats_no_frames_and_runs_on_as_it_shows() {
        let mut store = NodeStore::new();
        let mut cx = BuildCx::new(&mut store);
        let size = Sizing::fixed(100.0, 4.0);
        let bar = cx.progress(ProgressShape::Bar, size, BAR, BLUE, None).id();
        assert!(store.is_progressing());
        store.set_hidden(bar, true);
        assert!(!store.is_progressing(), "hidden, it beats no frames");
        store.tick_progress(Duration::from_millis(16));
        assert_eq!(store.progress(bar).unwrap().time, 0.0);
        store.set_hidden(bar, false);
        assert!(store.is_progressing());
        store.tick_progress(Duration::from_millis(16));
        assert!(store.progress(bar).unwrap().time > 0.0);
    }
}
