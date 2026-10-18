//! Path data to device-space polylines.

use viso_math::{Affine2, Vec2};
use viso_svg::tree::{PathData, Segment};

/// The most line segments one Bézier flattens to.
const MAX_CURVE_STEPS: f32 = 1024.0;

/// Flattened subpaths: polylines in device pixels.
#[derive(Default)]
pub(super) struct Polylines {
    points: Vec<Vec2>,
    /// Each subpath's exclusive end in `points`, and whether it is closed.
    ends: Vec<(usize, bool)>,
}

impl Polylines {
    /// Each subpath's points and whether it is closed; a closed subpath ends
    /// on its start point.
    pub(super) fn iter(&self) -> impl Iterator<Item = (&[Vec2], bool)> {
        let starts = core::iter::once(0).chain(self.ends.iter().map(|&(end, _)| end));
        starts
            .zip(self.ends.iter().copied())
            .map(|(start, (end, closed))| (&self.points[start..end], closed))
    }

    /// Whether no subpath has two distinct points.
    pub(super) fn is_empty(&self) -> bool {
        self.ends.is_empty()
    }

    /// Replaces the polylines with `data` mapped through `ts`, its curves
    /// flattened to within `tolerance` pixels. Consecutive duplicate points
    /// are dropped and a subpath left with fewer than two points is skipped.
    pub(super) fn flatten(&mut self, data: &PathData, ts: &Affine2, tolerance: f32) {
        self.points.clear();
        self.ends.clear();
        let map = |p: Vec2| ts.transform_point(p);
        let mut start = 0;
        let mut first = Vec2::ZERO;
        let mut closed = false;
        for segment in data.segments() {
            match *segment {
                Segment::MoveTo(p) => {
                    self.finish(start, closed);
                    start = self.points.len();
                    closed = false;
                    first = map(p);
                    self.points.push(first);
                }
                Segment::LineTo(p) => self.push(map(p)),
                Segment::QuadTo(c, p) => {
                    let (c, p) = (map(c), map(p));
                    let p0 = self.current(start).unwrap_or(c);
                    let dd = (p0 - c * 2.0 + p).length();
                    let n = steps((dd / (4.0 * tolerance)).sqrt());
                    for i in 1..n {
                        let t = i as f32 / n as f32;
                        let u = 1.0 - t;
                        self.push(p0 * (u * u) + c * (2.0 * u * t) + p * (t * t));
                    }
                    self.push(p);
                }
                Segment::CubicTo(c0, c1, p) => {
                    let (c0, c1, p) = (map(c0), map(c1), map(p));
                    let p0 = self.current(start).unwrap_or(c0);
                    let dd = (p0 - c0 * 2.0 + c1)
                        .length()
                        .max((c0 - c1 * 2.0 + p).length());
                    let n = steps((0.75 * dd / tolerance).sqrt());
                    for i in 1..n {
                        let t = i as f32 / n as f32;
                        let u = 1.0 - t;
                        self.push(
                            p0 * (u * u * u)
                                + c0 * (3.0 * u * u * t)
                                + c1 * (3.0 * u * t * t)
                                + p * (t * t * t),
                        );
                    }
                    self.push(p);
                }
                Segment::Close => {
                    closed = true;
                    self.push(first);
                }
            }
        }
        self.finish(start, closed);
    }

    /// The current subpath's last point, when it has one.
    fn current(&self, start: usize) -> Option<Vec2> {
        self.points[start..].last().copied()
    }

    fn push(&mut self, p: Vec2) {
        if self.points.last() != Some(&p) {
            self.points.push(p);
        }
    }

    /// Ends the subpath begun at `start`, dropping it when degenerate.
    fn finish(&mut self, start: usize, closed: bool) {
        if self.points.len() - start >= 2 {
            self.ends.push((self.points.len(), closed));
        } else {
            self.points.truncate(start);
        }
    }
}

/// Uniform parameter steps for a curve whose flatness bound asks for `n`.
fn steps(n: f32) -> u32 {
    n.ceil().clamp(1.0, MAX_CURVE_STEPS) as u32
}
