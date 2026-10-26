//! Plots: data drawn into a node's box — lines, areas, bars and dots over
//! gridlines, and a mark on what the pointer or the keyboard points at.
//!
//! A plot is a node's content ([`Content::Plot`]). Its layers hold their
//! points in the unit square — `x` from the start edge (`0`) to the end
//! (`1`), `y` from the bottom (`0`) to the top (`1`) — and paint maps them
//! into the node's box as it is laid out, so a resize repaints the plot
//! without rebuilding it, and strokes, dots and corners keep their size in
//! points. Read right to left the start edge is the right. A line of more
//! points than its box has pixel columns draws each column's first, lowest,
//! highest and last point, the same line to the eye at a fraction of the
//! work; a smooth line runs a monotone cubic through its points, never
//! overshooting them. A point whose `y` is not a number breaks the line or
//! the area there.

use std::sync::Arc;

use viso_render::{
    AnalyticRRect, Border, Corners, LineCap, LineJoin, Path, PathCmd, Point, Primitive, Quad, Rect,
    Rgba, Stroke,
};

use crate::component::{BuildCx, Handle, LeafStyle, NodeStore};
use crate::content::Content;
use crate::dirty::DirtyClass;
use crate::layout::Sizing;
use crate::node::NodeId;
use crate::style::BoxStyle;

/// A bar of a [`PlotLayer::Bars`]: in the band from `x0` to `x1` across,
/// the `slot`th of `slots` side by side, from its base `y0` to its value
/// `y1`, in the unit square; `round` rounds its value end (the outermost
/// segment of a stack, or a bar alone).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PlotBar {
    pub x0: f32,
    pub x1: f32,
    pub y0: f32,
    pub y1: f32,
    pub color: Rgba,
    pub round: bool,
    pub slot: u16,
    pub slots: u16,
}

/// One layer of a [`Plot`], painted over those before it.
#[derive(Debug, Clone, PartialEq)]
pub enum PlotLayer {
    /// A line through `points`, in order of `x`.
    Line {
        points: Arc<[Point]>,
        color: Rgba,
        width: f32,
        smooth: bool,
    },
    /// The region between `top` and `bottom` (same length, same `x`s), or
    /// down to `base` with no `bottom`, filled `fill`; `line` strokes `top`.
    Area {
        top: Arc<[Point]>,
        bottom: Option<Arc<[Point]>>,
        base: f32,
        fill: Rgba,
        line: Option<(Rgba, f32)>,
        smooth: bool,
    },
    /// Bars grown from `base`, their value ends rounded by `radius`, each no
    /// wider than `max_width`, those of a band side by side `gap` apart and
    /// centered in it, a segment of a stack `gap` above the one under it.
    Bars {
        bars: Arc<[PlotBar]>,
        base: f32,
        radius: f32,
        max_width: f32,
        gap: f32,
    },
    /// A dot of `radius` at each point, ringed `ring` (the background, so
    /// dots stay apart where they overlap), if given.
    Dots {
        points: Arc<[Point]>,
        radius: f32,
        color: Rgba,
        ring: Option<Rgba>,
    },
}

/// The gridlines under a plot's layers: across at each of `rows` (`y`s),
/// up at each of `columns` (`x`s), and a stronger line at `axis` (a `y`).
#[derive(Debug, Clone, PartialEq, Default)]
pub struct PlotGrid {
    pub rows: Arc<[f32]>,
    pub columns: Arc<[f32]>,
    pub color: Rgba,
    pub axis: Option<(f32, Rgba)>,
}

/// What a plot marks over its layers: a band behind them (`x0..x1`), a line
/// up at an `x`, and dots ringed `ring` (the background, to lift them off
/// the lines).
#[derive(Debug, Clone, PartialEq, Default)]
pub struct PlotMark {
    pub band: Option<(f32, f32)>,
    pub line: Option<f32>,
    pub color: Rgba,
    pub dots: Vec<(Point, Rgba)>,
    pub radius: f32,
    pub ring: Rgba,
}

/// What a plot node draws.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Plot {
    pub grid: PlotGrid,
    pub layers: Arc<[PlotLayer]>,
    pub mark: Option<PlotMark>,
}

/// How wide the ring round a dot is.
const RING: f32 = 2.0;

/// Maps the unit square into a box.
#[derive(Debug, Clone, Copy)]
struct Frame {
    world: Rect,
    rtl: bool,
}

impl Frame {
    fn x(&self, x: f32) -> f32 {
        let x = if self.rtl { 1.0 - x } else { x };
        self.world.x + x * self.world.w
    }

    fn y(&self, y: f32) -> f32 {
        self.world.y + (1.0 - y) * self.world.h
    }

    fn at(&self, p: Point) -> Point {
        Point::new(self.x(p.x), self.y(p.y))
    }
}

impl Plot {
    /// Draw the plot into `world`, its start edge the right one if `rtl`.
    pub(crate) fn paint(&self, world: Rect, rtl: bool, out: &mut Vec<Primitive>) {
        if world.w <= 0.0 || world.h <= 0.0 {
            return;
        }
        let frame = Frame { world, rtl };
        self.paint_grid(frame, out);
        if let Some(PlotMark {
            band: Some((x0, x1)),
            color,
            ..
        }) = &self.mark
        {
            let (a, b) = (frame.x(*x0), frame.x(*x1));
            out.push(quad(
                Rect {
                    x: a.min(b),
                    y: world.y,
                    w: (a - b).abs(),
                    h: world.h,
                },
                *color,
                0.0,
            ));
        }
        for layer in self.layers.iter() {
            paint_layer(layer, frame, out);
        }
        if let Some(mark) = &self.mark {
            if let Some(x) = mark.line {
                let x = (frame.x(x) - 0.5).round();
                out.push(quad(
                    Rect {
                        x,
                        y: world.y,
                        w: 1.0,
                        h: world.h,
                    },
                    mark.color,
                    0.0,
                ));
            }
            for &(p, color) in &mark.dots {
                if p.y.is_finite() {
                    out.push(dot(frame.at(p), mark.radius, color, Some(mark.ring)));
                }
            }
        }
    }

    fn paint_grid(&self, frame: Frame, out: &mut Vec<Primitive>) {
        let grid = &self.grid;
        let w = frame.world;
        if grid.color.a > 0.0 {
            for &y in grid.rows.iter() {
                out.push(quad(hline(frame, y), grid.color, 0.0));
            }
            for &x in grid.columns.iter() {
                let x = (frame.x(x) - 0.5).round();
                out.push(quad(
                    Rect {
                        x,
                        y: w.y,
                        w: 1.0,
                        h: w.h,
                    },
                    grid.color,
                    0.0,
                ));
            }
        }
        if let Some((y, color)) = grid.axis {
            out.push(quad(hline(frame, y), color, 0.0));
        }
    }
}

/// A hairline across the box at `y`, filling the pixel row it falls in.
fn hline(frame: Frame, y: f32) -> Rect {
    Rect {
        x: frame.world.x,
        y: (frame.y(y) - 0.5).round(),
        w: frame.world.w,
        h: 1.0,
    }
}

fn quad(rect: Rect, color: Rgba, radius: f32) -> Primitive {
    Primitive::Quad(Quad {
        rect,
        color,
        radius,
        border: Border {
            width: 0.0,
            color: Rgba::TRANSPARENT,
        },
    })
}

fn line_stroke(color: Rgba, width: f32) -> Stroke {
    Stroke {
        cap: LineCap::Round,
        join: LineJoin::Round,
        ..Stroke::new(width, color)
    }
}

fn paint_layer(layer: &PlotLayer, frame: Frame, out: &mut Vec<Primitive>) {
    match layer {
        PlotLayer::Line {
            points,
            color,
            width,
            smooth,
        } => {
            let mut cmds = Vec::new();
            for run in runs(points) {
                let run = shown(run, frame);
                trace(&run, *smooth, true, &mut cmds);
            }
            if !cmds.is_empty() {
                out.push(Primitive::Path(Box::new(Path {
                    cmds,
                    fill: None,
                    stroke: Some(line_stroke(*color, *width)),
                    shadow: None,
                })));
            }
        }
        PlotLayer::Area {
            top,
            bottom,
            base,
            fill,
            line,
            smooth,
        } => {
            let mut cmds = Vec::new();
            let mut edge = Vec::new();
            let mut start = 0;
            for run in runs(top) {
                // Where this run sits in `top`, for the matching `bottom`.
                let from = start
                    + top[start..]
                        .iter()
                        .position(|p| p.y.is_finite())
                        .unwrap_or(0);
                start = from + run.len();
                let upper = shown(run, frame);
                trace(&upper, *smooth, true, &mut cmds);
                let lower: Vec<Point> = match bottom {
                    Some(bottom) => {
                        let low = &bottom[from..(from + run.len()).min(bottom.len())];
                        let mut low = shown(low, frame);
                        low.reverse();
                        low
                    }
                    None => {
                        let y = frame.y(*base);
                        let (first, last) = (upper[0], upper[upper.len() - 1]);
                        vec![Point::new(last.x, y), Point::new(first.x, y)]
                    }
                };
                trace(&lower, *smooth && bottom.is_some(), false, &mut cmds);
                cmds.push(PathCmd::Close);
                if line.is_some() {
                    trace(&upper, *smooth, true, &mut edge);
                }
            }
            if !cmds.is_empty() {
                out.push(Primitive::Path(Box::new(Path {
                    cmds,
                    fill: Some(*fill),
                    stroke: None,
                    shadow: None,
                })));
            }
            if let Some((color, width)) = *line
                && !edge.is_empty()
            {
                out.push(Primitive::Path(Box::new(Path {
                    cmds: edge,
                    fill: None,
                    stroke: Some(line_stroke(color, width)),
                    shadow: None,
                })));
            }
        }
        PlotLayer::Bars {
            bars,
            base,
            radius,
            max_width,
            gap,
        } => {
            let base_y = frame.y(*base);
            for bar in bars.iter() {
                let (x0, x1) = (frame.x(bar.x0), frame.x(bar.x1));
                let (y0, y1) = (frame.y(bar.y0), frame.y(bar.y1));
                // The band's bars side by side, no wider than the most a
                // bar is, centered in it; read right to left the first is
                // the right one.
                let (left, right) = (x0.min(x1), x0.max(x1));
                let slots = f32::from(bar.slots.max(1));
                let w = ((right - left - gap * (slots - 1.0)) / slots)
                    .min(*max_width)
                    .max(0.0);
                let group = w * slots + gap * (slots - 1.0);
                let slot = if frame.rtl {
                    slots - 1.0 - f32::from(bar.slot)
                } else {
                    f32::from(bar.slot)
                };
                let x = (left + right - group) * 0.5 + slot * (w + gap);
                // A segment off the base leaves a gap under it.
                let up = y1 <= y0;
                let off_base = (y0 - base_y).abs() > 0.5;
                let inset = if off_base { *gap } else { 0.0 };
                let (top, bottom) = if up {
                    (y1, y0 - inset)
                } else {
                    (y0 + inset, y1)
                };
                let rect = Rect {
                    x,
                    y: top,
                    w,
                    h: bottom - top,
                };
                if rect.w <= 0.0 || rect.h <= 0.0 {
                    continue;
                }
                let r = if bar.round {
                    radius.min(rect.w * 0.5).min(rect.h)
                } else {
                    0.0
                };
                // The value end is rounded: the top of a bar rising.
                let radius = if up {
                    Corners {
                        left_top: r,
                        right_top: r,
                        right_bottom: 0.0,
                        left_bottom: 0.0,
                    }
                } else {
                    Corners {
                        left_top: 0.0,
                        right_top: 0.0,
                        right_bottom: r,
                        left_bottom: r,
                    }
                };
                out.push(Primitive::AnalyticRRect(AnalyticRRect {
                    rect,
                    color: bar.color,
                    radius,
                    border: Border {
                        width: 0.0,
                        color: Rgba::TRANSPARENT,
                    },
                }));
            }
        }
        PlotLayer::Dots {
            points,
            radius,
            color,
            ring,
        } => {
            for &p in points.iter() {
                if !p.y.is_finite() || !p.x.is_finite() {
                    continue;
                }
                out.push(dot(frame.at(p), *radius, *color, *ring));
            }
        }
    }
}

/// A dot of `r` at `c`, ringed 2 points of `ring` outside it.
fn dot(c: Point, r: f32, color: Rgba, ring: Option<Rgba>) -> Primitive {
    let (outer, border) = match ring {
        Some(ring) => (
            r + RING,
            Border {
                width: RING,
                color: ring,
            },
        ),
        None => (
            r,
            Border {
                width: 0.0,
                color: Rgba::TRANSPARENT,
            },
        ),
    };
    Primitive::Quad(Quad {
        rect: Rect {
            x: c.x - outer,
            y: c.y - outer,
            w: 2.0 * outer,
            h: 2.0 * outer,
        },
        color,
        radius: outer,
        border,
    })
}

/// The stretches of `points` whose `y`s are numbers.
fn runs(points: &[Point]) -> impl Iterator<Item = &[Point]> {
    points
        .split(|p| !p.y.is_finite() || !p.x.is_finite())
        .filter(|run| !run.is_empty())
}

/// `run` mapped into the box, each pixel column of a run denser than the
/// box keeping its first, lowest, highest and last point.
fn shown(run: &[Point], frame: Frame) -> Vec<Point> {
    let columns = frame.world.w.ceil().max(1.0) as usize;
    if run.len() <= 4 * columns {
        return run.iter().map(|&p| frame.at(p)).collect();
    }
    let mut out = Vec::with_capacity(4 * columns + 4);
    let mut k = 0;
    while k < run.len() {
        let column = (run[k].x * columns as f32).floor();
        let (mut low, mut high) = (k, k);
        let mut end = k + 1;
        while end < run.len() && (run[end].x * columns as f32).floor() == column {
            if run[end].y < run[low].y {
                low = end;
            }
            if run[end].y > run[high].y {
                high = end;
            }
            end += 1;
        }
        let last = end - 1;
        let (a, b) = if low < high { (low, high) } else { (high, low) };
        for i in [k, a, b, last] {
            if out.last() != Some(&frame.at(run[i])) {
                out.push(frame.at(run[i]));
            }
        }
        k = end;
    }
    out
}

/// Append the line through `pts` to `cmds`: a new subpath if `start`, else
/// on from where the last left off; through a monotone cubic if `smooth`
/// and the points run one way across.
fn trace(pts: &[Point], smooth: bool, start: bool, cmds: &mut Vec<PathCmd>) {
    let Some(&first) = pts.first() else {
        return;
    };
    cmds.push(if start {
        PathCmd::MoveTo(first)
    } else {
        PathCmd::LineTo(first)
    });
    if pts.len() == 1 {
        // A lone point still shows: a stroke's round caps draw a dot.
        if start {
            cmds.push(PathCmd::LineTo(first));
        }
        return;
    }
    let one_way =
        pts.windows(2).all(|w| w[1].x > w[0].x) || pts.windows(2).all(|w| w[1].x < w[0].x);
    if !smooth || !one_way || pts.len() < 3 {
        cmds.extend(pts[1..].iter().map(|&p| PathCmd::LineTo(p)));
        return;
    }
    let slopes = monotone_slopes(pts);
    for k in 0..pts.len() - 1 {
        let (p0, p1) = (pts[k], pts[k + 1]);
        let third = (p1.x - p0.x) / 3.0;
        cmds.push(PathCmd::CubicTo(
            Point::new(p0.x + third, p0.y + third * slopes[k]),
            Point::new(p1.x - third, p1.y - third * slopes[k + 1]),
            p1,
        ));
    }
}

/// The tangent slope at each point of a monotone cubic through `pts`
/// (`x`s strictly one way): flat at a peak or a trough, so the curve never
/// overshoots a point.
fn monotone_slopes(pts: &[Point]) -> Vec<f32> {
    let n = pts.len();
    let secant = |k: usize| (pts[k + 1].y - pts[k].y) / (pts[k + 1].x - pts[k].x);
    let mut m = vec![0.0; n];
    for k in 1..n - 1 {
        let (h0, h1) = (pts[k].x - pts[k - 1].x, pts[k + 1].x - pts[k].x);
        let (s0, s1) = (secant(k - 1), secant(k));
        let p = (s0 * h1 + s1 * h0) / (h0 + h1);
        m[k] = (s0.signum() + s1.signum()) * s0.abs().min(s1.abs()).min(0.5 * p.abs());
    }
    // The ends lean as the secant there does, against the next tangent.
    let end = |s: f32, t: f32| (3.0 * s - t) / 2.0;
    m[0] = end(secant(0), m[1]);
    m[n - 1] = end(secant(n - 2), m[n - 2]);
    m
}

impl BuildCx<'_> {
    /// A plot leaf asking `size` of its parent and drawing `plot` (see
    /// [`NodeStore::set_plot`]).
    pub fn plot(&mut self, size: Sizing, plot: Plot) -> Handle {
        let handle = self.leaf(LeafStyle {
            size,
            style: BoxStyle::NONE,
        });
        self.store.set_plot(handle.id(), plot);
        handle
    }
}

impl NodeStore {
    /// What node `id` plots, if it is a plot.
    pub fn plot(&self, id: NodeId) -> Option<&Plot> {
        match self.content_payload(id) {
            Some(Content::Plot(plot)) => Some(plot),
            _ => None,
        }
    }

    /// Make node `id` draw `plot`, repainting it (`PAINT`). A plot sizes to
    /// nothing of its own: its leaf's size sets its box.
    pub fn set_plot(&mut self, id: NodeId, plot: Plot) {
        if !self.arena().is_live(id) || self.plot(id) == Some(&plot) {
            return;
        }
        if self.plot(id).is_none() {
            self.mark_dirty(id, DirtyClass::MEASURE);
        }
        self.set_content_payload(id, Content::Plot(Box::new(plot)));
        self.mark_dirty(id, DirtyClass::PAINT);
    }

    /// Mark `mark` over plot `id`'s layers, or nothing, repainting it
    /// (`PAINT`) when that changed.
    pub fn set_plot_mark(&mut self, id: NodeId, mark: Option<PlotMark>) {
        let Some(plot) = self.plot(id) else {
            return;
        };
        if plot.mark == mark {
            return;
        }
        let plot = Plot {
            mark,
            grid: plot.grid.clone(),
            layers: Arc::clone(&plot.layers),
        };
        self.set_content_payload(id, Content::Plot(Box::new(plot)));
        self.mark_dirty(id, DirtyClass::PAINT);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const INK: Rgba = Rgba::new(0.0, 0.0, 0.0, 1.0);

    fn frame(w: f32, h: f32) -> Frame {
        Frame {
            world: Rect {
                x: 10.0,
                y: 20.0,
                w,
                h,
            },
            rtl: false,
        }
    }

    #[test]
    fn the_unit_square_maps_into_the_box_bottom_up_and_mirrored_right_to_left() {
        let f = frame(100.0, 50.0);
        assert_eq!(f.at(Point::new(0.0, 0.0)), Point::new(10.0, 70.0));
        assert_eq!(f.at(Point::new(1.0, 1.0)), Point::new(110.0, 20.0));
        let f = Frame { rtl: true, ..f };
        assert_eq!(f.at(Point::new(0.0, 0.0)), Point::new(110.0, 70.0));
    }

    #[test]
    fn a_dense_line_keeps_each_columns_extremes() {
        let points: Vec<Point> = (0..10_000)
            .map(|i| {
                let x = i as f32 / 9_999.0;
                Point::new(x, if i % 2 == 0 { 0.0 } else { 1.0 })
            })
            .collect();
        let f = frame(100.0, 50.0);
        let shown = shown(&points, f);
        assert!(shown.len() <= 4 * 101, "{}", shown.len());
        let ys: Vec<f32> = shown.iter().map(|p| p.y).collect();
        assert!(
            ys.contains(&20.0) && ys.contains(&70.0),
            "both extremes kept"
        );
    }

    #[test]
    fn a_smooth_line_never_overshoots_and_a_gap_breaks_it() {
        let pts = [
            Point::new(0.0, 0.0),
            Point::new(1.0, 10.0),
            Point::new(2.0, 10.0),
            Point::new(3.0, 0.0),
        ];
        let m = monotone_slopes(&pts);
        assert_eq!(m[1], 0.0, "flat on a level stretch");
        assert_eq!(m[2], 0.0);
        let mut cmds = Vec::new();
        trace(&pts, true, true, &mut cmds);
        for c in &cmds {
            if let PathCmd::CubicTo(a, b, _) = c {
                assert!(a.y <= 10.0 && b.y <= 10.0 && a.y >= 0.0 && b.y >= 0.0);
            }
        }
        let gap = [
            Point::new(0.0, 0.1),
            Point::new(0.1, f32::NAN),
            Point::new(0.2, 0.3),
            Point::new(0.3, 0.4),
        ];
        assert_eq!(runs(&gap).count(), 2);
    }

    #[test]
    fn a_plot_paints_its_grid_its_layers_and_its_mark() {
        let plot = Plot {
            grid: PlotGrid {
                rows: Arc::from([0.0, 0.5, 1.0]),
                columns: Arc::from([]),
                color: INK,
                axis: None,
            },
            layers: Arc::from([
                PlotLayer::Area {
                    top: Arc::from([Point::new(0.0, 0.5), Point::new(1.0, 1.0)]),
                    bottom: None,
                    base: 0.0,
                    fill: INK,
                    line: Some((INK, 2.0)),
                    smooth: false,
                },
                PlotLayer::Bars {
                    bars: Arc::from([PlotBar {
                        x0: 0.1,
                        x1: 0.2,
                        y0: 0.0,
                        y1: 0.5,
                        color: INK,
                        round: true,
                        slot: 0,
                        slots: 1,
                    }]),
                    base: 0.0,
                    radius: 3.0,
                    max_width: 24.0,
                    gap: 2.0,
                },
            ]),
            mark: Some(PlotMark {
                line: Some(0.5),
                dots: vec![(Point::new(0.5, 0.75), INK)],
                radius: 4.0,
                ..PlotMark::default()
            }),
        };
        let mut out = Vec::new();
        plot.paint(
            Rect {
                x: 0.0,
                y: 0.0,
                w: 200.0,
                h: 100.0,
            },
            false,
            &mut out,
        );
        let kinds: Vec<&str> = out.iter().map(|p| p.label()).collect();
        assert_eq!(
            kinds,
            [
                "quad", "quad", "quad", "path", "path", "rrect", "quad", "quad"
            ]
        );
        let Primitive::AnalyticRRect(bar) = &out[5] else {
            unreachable!()
        };
        assert_eq!(
            (bar.rect.x, bar.rect.y, bar.rect.w, bar.rect.h),
            (20.0, 50.0, 20.0, 50.0)
        );
        assert_eq!(bar.radius.left_top, 3.0);
        assert_eq!(bar.radius.left_bottom, 0.0);
    }
}
