//! A stroke as the union of consistently oriented polygons, expanded the way
//! the renderer's tessellator expands it: a quad per segment, a wedge on the
//! outer side of each corner, a cap at each open end, and every dash an open
//! polyline of its own.

use core::f32::consts::PI;

use viso_math::Vec2;
use viso_svg::tree::LineCap;

use super::coverage::Outline;
use super::flatten::Polylines;

/// The most dash lengths a pattern keeps, as the renderer's dash pattern does.
const DASH_RUNS_MAX: usize = 4;

/// Turns sharper than this cross product (of unit directions) get a join.
const STRAIGHT: f32 = 1e-4;

/// How a corner is filled. SVG 2's `miter-clip` draws as a plain miter.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Join {
    Miter,
    Round,
    Bevel,
}

/// A dash pattern in device pixels: on/off lengths from an "on" run, and the
/// phase it starts at.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(super) struct Dash {
    runs: [f32; DASH_RUNS_MAX],
    len: usize,
    offset: f32,
}

impl Dash {
    /// The first [`DASH_RUNS_MAX`] of `lengths`, each times `scale`, starting
    /// `offset` into the pattern.
    pub(super) fn new(lengths: &[f32], offset: f32, scale: f32) -> Dash {
        let mut runs = [0.0; DASH_RUNS_MAX];
        let len = lengths.len().min(DASH_RUNS_MAX);
        for (run, &length) in runs.iter_mut().zip(lengths) {
            *run = length * scale;
        }
        Dash {
            runs,
            len,
            offset: offset * scale,
        }
    }

    fn runs(&self) -> &[f32] {
        &self.runs[..self.len]
    }
}

/// The shape a stroke draws with, in device pixels.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(super) struct Pen {
    pub half_width: f32,
    pub cap: LineCap,
    pub join: Join,
    /// The longest miter, as a multiple of the half-width, before it bevels.
    pub miter_limit: f32,
    pub dash: Option<Dash>,
    /// How far a flattened round cap or join may fall inside its arc.
    pub tolerance: f32,
}

/// Scratch buffers reused across strokes.
#[derive(Default)]
pub(super) struct Stroker {
    /// The dash runs of the current subpath, and each run's end.
    dashes: Vec<Vec2>,
    dash_ends: Vec<usize>,
    /// The current polyline without repeated points.
    ring: Vec<Vec2>,
    /// The current round cap or join.
    arc: Vec<Vec2>,
}

impl Stroker {
    /// Appends `lines` stroked with `pen` to `out`, every piece of positive
    /// area so their nonzero fill is the stroke's union.
    pub(super) fn stroke(&mut self, lines: &Polylines, pen: &Pen, out: &mut Outline) {
        let Stroker {
            dashes,
            dash_ends,
            ring,
            arc,
        } = self;
        let mut scratch = Scratch { ring, arc };
        for (points, closed) in lines.iter() {
            let points = match points.split_last() {
                Some((last, head)) if closed && head.first() == Some(last) => head,
                _ => points,
            };
            match pen.dash {
                Some(dash) if dash.len > 0 => {
                    if dash.runs().iter().sum::<f32>() <= 0.0 {
                        scratch.polyline(points, false, pen, out);
                        continue;
                    }
                    dash_runs(points, closed, &dash, dashes, dash_ends);
                    let mut start = 0;
                    for &end in dash_ends.iter() {
                        scratch.polyline(&dashes[start..end], false, pen, out);
                        start = end;
                    }
                }
                _ => scratch.polyline(points, closed, pen, out),
            }
        }
    }
}

/// The buffers one polyline is expanded with.
struct Scratch<'a> {
    ring: &'a mut Vec<Vec2>,
    arc: &'a mut Vec<Vec2>,
}

impl Scratch<'_> {
    /// One polyline (a ring when `closed`) expanded into `out`.
    fn polyline(&mut self, points: &[Vec2], closed: bool, pen: &Pen, out: &mut Outline) {
        let hw = pen.half_width;
        self.ring.clear();
        for &p in points {
            if self.ring.last() != Some(&p) {
                self.ring.push(p);
            }
        }
        if closed {
            while self.ring.len() > 1 && self.ring.last() == self.ring.first() {
                self.ring.pop();
            }
        }
        let ring = &*self.ring;
        let count = ring.len();
        if count < 2 || hw.is_nan() || hw <= 0.0 {
            return;
        }
        let segments = if closed { count } else { count - 1 };
        for s in 0..segments {
            let a = ring[s];
            let b = ring[(s + 1) % count];
            let normal = left_normal((b - a).normalize());
            out.push_oriented(&[
                a + normal * hw,
                b + normal * hw,
                b - normal * hw,
                a - normal * hw,
            ]);
            if closed || s + 1 < segments {
                let c = ring[(s + 2) % count];
                join(a, b, c, pen, self.arc, out);
            }
        }
        if !closed {
            let (first, last) = (ring[0], ring[count - 1]);
            cap(first, (first - ring[1]).normalize(), pen, self.arc, out);
            cap(
                last,
                (last - ring[count - 2]).normalize(),
                pen,
                self.arc,
                out,
            );
        }
    }
}

/// The wedge filling corner `b` between `a → b` and `b → c` on its outer side.
fn join(a: Vec2, b: Vec2, c: Vec2, pen: &Pen, arc: &mut Vec<Vec2>, out: &mut Outline) {
    let hw = pen.half_width;
    let incoming = (b - a).normalize();
    let outgoing = (c - b).normalize();
    let cross = incoming.cross(outgoing);
    if cross.abs() < STRAIGHT {
        return;
    }
    // The outer side is opposite the turn.
    let side = if cross > 0.0 { -hw } else { hw };
    let (pn, nn) = (left_normal(incoming), left_normal(outgoing));
    let from = b + pn * side;
    let to = b + nn * side;
    match pen.join {
        Join::Round => {
            let start = angle(from - b);
            let mut sweep = angle(to - b) - start;
            while sweep > PI {
                sweep -= 2.0 * PI;
            }
            while sweep < -PI {
                sweep += 2.0 * PI;
            }
            arc.clear();
            arc.push(b);
            push_arc(arc, b, hw, start, sweep, pen.tolerance);
            out.push_oriented(arc);
        }
        Join::Miter => match miter_apex(from, incoming, to, outgoing) {
            Some(apex) if pn.dot(nn) < 0.999 && (apex - b).length() <= pen.miter_limit * hw => {
                out.push_oriented(&[b, from, apex, to]);
            }
            _ => out.push_oriented(&[b, from, to]),
        },
        Join::Bevel => out.push_oriented(&[b, from, to]),
    }
}

/// The cap at open end `end`, `dir` pointing away from the polyline.
fn cap(end: Vec2, dir: Vec2, pen: &Pen, arc: &mut Vec<Vec2>, out: &mut Outline) {
    let hw = pen.half_width;
    if dir == Vec2::ZERO {
        return;
    }
    let normal = left_normal(dir);
    let (l, r) = (end + normal * hw, end - normal * hw);
    match pen.cap {
        LineCap::Butt => {}
        LineCap::Square => out.push_oriented(&[l, l + dir * hw, r + dir * hw, r]),
        LineCap::Round => {
            arc.clear();
            let start = angle(normal);
            let sweep = if normal.cross(dir) > 0.0 { PI } else { -PI };
            push_arc(arc, end, hw, start, sweep, pen.tolerance);
            out.push_oriented(arc);
        }
    }
}

/// Points of the arc of radius `r` about `center` from angle `start` through
/// `sweep`, both ends included, each chord within `tolerance` of the arc.
fn push_arc(arc: &mut Vec<Vec2>, center: Vec2, r: f32, start: f32, sweep: f32, tolerance: f32) {
    let step = 2.0 * (1.0 - tolerance / r).max(-1.0).acos();
    let steps = (sweep.abs() / step.max(1e-3)).ceil().clamp(1.0, 4096.0) as u32;
    for i in 0..=steps {
        let angle = start + sweep * (i as f32 / steps as f32);
        arc.push(center + Vec2::new(angle.cos(), angle.sin()) * r);
    }
}

/// `dir` turned a quarter turn: the side the renderer calls left.
fn left_normal(dir: Vec2) -> Vec2 {
    Vec2::new(-dir.y, dir.x)
}

/// The direction of `v` as an angle from `+x`.
fn angle(v: Vec2) -> f32 {
    v.y.atan2(v.x)
}

/// Where the line through `p` along `pdir` meets the line through `q` along
/// `qdir`; `None` when they are near parallel.
fn miter_apex(p: Vec2, pdir: Vec2, q: Vec2, qdir: Vec2) -> Option<Vec2> {
    let denom = pdir.cross(qdir);
    if denom.abs() < 1e-6 {
        return None;
    }
    let t = (q - p).cross(qdir) / denom;
    Some(p + pdir * t)
}

/// Splits the centerline `ring` (closed back to its start when `closed`) into
/// the "on" runs of `dash`, each an open polyline, into `points` with each
/// run's end in `ends`.
fn dash_runs(
    ring: &[Vec2],
    closed: bool,
    dash: &Dash,
    points: &mut Vec<Vec2>,
    ends: &mut Vec<usize>,
) {
    points.clear();
    ends.clear();
    let runs = dash.runs();
    let period: f32 = runs.iter().sum();
    let count = ring.len() + usize::from(closed && ring.len() >= 2);
    if count < 2 {
        return;
    }
    let at = |i: usize| ring[i % ring.len()];

    let mut phase = dash.offset.rem_euclid(period);
    let (mut idx, mut on) = (0, true);
    let mut acc = 0.0;
    while acc + runs[idx] <= phase && idx + 1 < runs.len() {
        acc += runs[idx];
        idx += 1;
        on = !on;
    }
    phase -= acc;
    let mut remaining = runs[idx] - phase;

    let mut run_start = 0;
    if on {
        points.push(at(0));
    }
    for i in 0..count - 1 {
        let (a, b) = (at(i), at(i + 1));
        let length = (b - a).length();
        if length.is_nan() || length <= 0.0 {
            continue;
        }
        let dir = (b - a) / length;
        let mut travelled = 0.0;
        while length - travelled > remaining {
            travelled += remaining;
            points.push(a + dir * travelled);
            if on {
                ends.push(points.len());
                run_start = points.len();
            }
            idx = (idx + 1) % runs.len();
            on = !on;
            remaining = runs[idx];
        }
        remaining -= length - travelled;
        if on {
            points.push(b);
        }
    }
    if on && points.len() - run_start >= 2 {
        ends.push(points.len());
    } else {
        points.truncate(run_start);
    }
}
