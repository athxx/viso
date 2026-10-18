//! Closed polygons to an anti-aliased coverage window: the exact signed-area
//! accumulation of `ab_glyph_rasterizer`, folded by a fill rule.
//!
//! The accumulated value at a pixel is the winding-weighted area of the
//! polygons over it, so nonzero clamps its magnitude to one and even-odd folds
//! it as a triangle wave of period two; both are exact wherever no two edges
//! share a pixel, and a union of consistently oriented polygons fills by
//! nonzero without any piece blending twice.

use ab_glyph_rasterizer::{Rasterizer, point};
use viso_math::Vec2;
use viso_svg::tree::FillRule;

/// Polygons in device pixels, each implicitly closed back to its first point.
#[derive(Default)]
pub(super) struct Outline {
    points: Vec<Vec2>,
    /// The exclusive end of each polygon in `points`.
    ends: Vec<usize>,
}

impl Outline {
    pub(super) fn clear(&mut self) {
        self.points.clear();
        self.ends.clear();
    }

    /// Appends `points` as one polygon, in their own order.
    pub(super) fn push(&mut self, points: &[Vec2]) {
        if points.len() >= 3 {
            self.points.extend_from_slice(points);
            self.ends.push(self.points.len());
        }
    }

    /// Appends `points` as one polygon of positive signed area, reversed when
    /// they wind the other way; a degenerate polygon adds nothing.
    pub(super) fn push_oriented(&mut self, points: &[Vec2]) {
        let area = signed_area(points);
        if area.is_nan() || area.abs() <= 1e-9 {
            return;
        }
        let start = self.points.len();
        self.push(points);
        if area < 0.0 {
            self.points[start..].reverse();
        }
    }

    fn polygons(&self) -> impl Iterator<Item = &[Vec2]> {
        let starts = core::iter::once(0).chain(self.ends.iter().copied());
        starts
            .zip(self.ends.iter().copied())
            .map(|(start, end)| &self.points[start..end])
    }
}

/// Twice the signed area of a polygon, positive when it turns clockwise on a
/// y-down screen.
fn signed_area(points: &[Vec2]) -> f32 {
    let Some(&last) = points.last() else {
        return 0.0;
    };
    let mut prev = last;
    let mut sum = 0.0;
    for &p in points {
        sum += prev.cross(p);
        prev = p;
    }
    sum
}

/// The pixel rectangle a coverage window spans.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct Window {
    pub x: u32,
    pub y: u32,
    pub width: u32,
    pub height: u32,
}

/// The reusable accumulation buffers.
#[derive(Default)]
pub(super) struct Coverage {
    rasterizer: Option<Rasterizer>,
    mask: Vec<f32>,
}

impl Coverage {
    /// `outline`'s coverage under `rule` over the pixels of a
    /// `width × height` image it touches: the window and its coverage in
    /// `[0, 1]`, row by row. `None` when it touches no pixel.
    pub(super) fn rasterize(
        &mut self,
        outline: &Outline,
        rule: FillRule,
        width: u32,
        height: u32,
    ) -> Option<(Window, &[f32])> {
        let window = window(outline, width, height)?;
        let (w, h) = (window.width as usize, window.height as usize);
        // Two spare columns per row take the cells of edges projected onto the
        // right border, so every row's cells sum to zero inside the row.
        let stride = w + 2;
        let rasterizer = match &mut self.rasterizer {
            Some(r) => {
                r.reset(stride, h);
                r
            }
            slot => slot.insert(Rasterizer::new(stride, h)),
        };
        let origin = Vec2::new(window.x as f32, window.y as f32);
        let right = w as f32;
        for polygon in outline.polygons() {
            let mut prev = polygon[polygon.len() - 1] - origin;
            for &p in polygon {
                let p = p - origin;
                draw_clipped(rasterizer, prev, p, right);
                prev = p;
            }
        }
        self.mask.clear();
        self.mask.resize(w * h, 0.0);
        let mask = &mut self.mask;
        rasterizer.for_each_pixel(|i, value| {
            let (x, y) = (i % stride, i / stride);
            if x < w {
                mask[y * w + x] = fold(value, rule);
            }
        });
        Some((window, &self.mask))
    }
}

/// The accumulated coverage magnitude `value` under `rule`.
fn fold(value: f32, rule: FillRule) -> f32 {
    match rule {
        FillRule::NonZero => value.min(1.0),
        FillRule::EvenOdd => {
            let m = value % 2.0;
            if m > 1.0 { 2.0 - m } else { m }
        }
    }
}

/// The pixels of a `width × height` image `outline`'s bounds touch.
fn window(outline: &Outline, width: u32, height: u32) -> Option<Window> {
    let (mut min, mut max) = (Vec2::splat(f32::INFINITY), Vec2::splat(f32::NEG_INFINITY));
    for &p in &outline.points {
        min = Vec2::new(min.x.min(p.x), min.y.min(p.y));
        max = Vec2::new(max.x.max(p.x), max.y.max(p.y));
    }
    let clamp = |v: f32, hi: u32| v.clamp(0.0, hi as f32) as u32;
    let (x0, x1) = (clamp(min.x.floor(), width), clamp(max.x.ceil(), width));
    let (y0, y1) = (clamp(min.y.floor(), height), clamp(max.y.ceil(), height));
    (x0 < x1 && y0 < y1).then(|| Window {
        x: x0,
        y: y0,
        width: x1 - x0,
        height: y1 - y0,
    })
}

/// Adds the edge `a → b` with every part left of `x = 0` or right of
/// `x = right` projected onto that border: outside the window an edge only
/// carries its winding across the row, never area.
fn draw_clipped(rasterizer: &mut Rasterizer, a: Vec2, b: Vec2, right: f32) {
    let mut cuts = [0.0, 1.0, 1.0, 1.0];
    let mut n = 1;
    let dx = b.x - a.x;
    if dx != 0.0 {
        for border in [0.0, right] {
            let t = (border - a.x) / dx;
            if t > 0.0 && t < 1.0 {
                cuts[n] = t;
                n += 1;
            }
        }
    }
    cuts[n] = 1.0;
    if n == 3 && cuts[1] > cuts[2] {
        cuts.swap(1, 2);
    }
    let at = |t: f32| {
        let p = if t <= 0.0 {
            a
        } else if t >= 1.0 {
            b
        } else {
            a + (b - a) * t
        };
        point(p.x.clamp(0.0, right), p.y)
    };
    for pair in cuts[..=n].windows(2) {
        let (p, q) = (at(pair[0]), at(pair[1]));
        rasterizer.draw_line(p, q);
    }
}
