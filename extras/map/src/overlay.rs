//! Shapes an app draws over the map — polylines, polygons and circles
//! measured on the ground — and the position puck.
//!
//! A shape is built once into a mesh in its own frame: its bounds' north
//! west corner at the origin, measured in the points of a zoom chosen so the
//! shape spans about a thousand of them. The view places the mesh like a
//! tile, scaling the frame to the zoom shown, so a pan or zoom only moves
//! it; its line widths are extrusion and stay the same on screen.

use viso_render::{Mesh, Primitive};
use viso_ui::{Drawing, Vec2};

use crate::camera::TILE_SIZE;
use crate::geo::{LngLat, WorldPoint};
use crate::style::Color;
use crate::tess::clip::Square;
use crate::tess::{Cap, Join, LineStyle, MeshBuilder};

/// About how many points a shape's frame spans.
const FRAME_SPAN: f64 = 1024.0;
/// The sides of a circle measured on the ground.
const CIRCLE_SIDES: usize = 72;

/// A line's stroke: its width in points and its color.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Stroke {
    pub width: f32,
    pub color: Color,
}

impl Stroke {
    /// A stroke `width` points wide in `color`.
    pub const fn new(width: f32, color: Color) -> Self {
        Self { width, color }
    }
}

/// A line through places, such as a route: drawn over an optional wider
/// casing, the part already traveled in its own colors.
#[derive(Debug, Clone, PartialEq)]
pub struct Polyline {
    pub points: Vec<LngLat>,
    pub stroke: Stroke,
    pub casing: Option<Stroke>,
    /// How much of the line, from its start, was traveled: `0.0` to `1.0`
    /// of its length.
    pub traveled: f64,
    /// The traveled part's stroke and casing colors.
    pub traveled_color: Color,
    pub traveled_casing: Color,
    pub join: Join,
    pub cap: Cap,
}

impl Polyline {
    /// A line through `points`, `width` points wide in `color`, with round
    /// joins and caps.
    pub fn new(points: impl Into<Vec<LngLat>>, width: f32, color: Color) -> Self {
        Self {
            points: points.into(),
            stroke: Stroke::new(width, color),
            casing: None,
            traveled: 0.0,
            traveled_color: Color::rgb(0xa8, 0xb0, 0xbc),
            traveled_casing: Color::rgb(0x80, 0x88, 0x94),
            join: Join::Round,
            cap: Cap::Round,
        }
    }

    /// The route look: a blue line over a darker casing.
    pub fn route(points: impl Into<Vec<LngLat>>) -> Self {
        Self::new(points, 6.0, Color::rgb(0x33, 0x82, 0xf2))
            .casing(Stroke::new(9.0, Color::rgb(0x10, 0x45, 0x8c)))
    }

    /// Draws `casing` beneath the line; its width is the whole casing's.
    pub fn casing(mut self, casing: Stroke) -> Self {
        self.casing = Some(casing);
        self
    }

    /// Shows the first `fraction` of the line as traveled.
    pub fn traveled(mut self, fraction: f64) -> Self {
        self.traveled = fraction.clamp(0.0, 1.0);
        self
    }

    /// Draws the traveled part in `color` over `casing`.
    pub fn traveled_colors(mut self, color: Color, casing: Color) -> Self {
        self.traveled_color = color;
        self.traveled_casing = casing;
        self
    }
}

/// An area bounded by rings of places: the outer ring first, then its
/// holes.
#[derive(Debug, Clone, PartialEq)]
pub struct Polygon {
    pub rings: Vec<Vec<LngLat>>,
    pub fill: Color,
    pub outline: Option<Stroke>,
}

impl Polygon {
    /// The area inside `ring`, filled with `fill`.
    pub fn new(ring: impl Into<Vec<LngLat>>, fill: Color) -> Self {
        Self {
            rings: vec![ring.into()],
            fill,
            outline: None,
        }
    }

    /// Cuts `ring` out of it.
    pub fn hole(mut self, ring: impl Into<Vec<LngLat>>) -> Self {
        self.rings.push(ring.into());
        self
    }

    /// Strokes its rings with `outline`.
    pub fn outline(mut self, outline: Stroke) -> Self {
        self.outline = Some(outline);
        self
    }
}

/// A circle measured on the ground: everything within `radius` meters of
/// `center`.
#[derive(Debug, Clone, PartialEq)]
pub struct Circle {
    pub center: LngLat,
    pub radius: f64,
    pub fill: Color,
    pub outline: Option<Stroke>,
}

impl Circle {
    /// The places within `radius` meters of `center`, filled with `fill`.
    pub fn new(center: LngLat, radius: f64, fill: Color) -> Self {
        Self {
            center,
            radius,
            fill,
            outline: None,
        }
    }

    /// Strokes its edge with `outline`.
    pub fn outline(mut self, outline: Stroke) -> Self {
        self.outline = Some(outline);
        self
    }

    /// Its edge as a ring of places.
    pub fn ring(&self) -> Vec<LngLat> {
        let c = self.center.to_world();
        let r = self.radius / c.meters_per_unit();
        (0..CIRCLE_SIDES)
            .map(|i| {
                let a = std::f64::consts::TAU * i as f64 / CIRCLE_SIDES as f64;
                WorldPoint::new(c.x + r * a.cos(), c.y + r * a.sin()).to_lng_lat()
            })
            .collect()
    }
}

/// Something drawn over the map.
#[derive(Debug, Clone, PartialEq)]
pub enum Shape {
    Line(Polyline),
    Polygon(Polygon),
    Circle(Circle),
}

impl From<Polyline> for Shape {
    fn from(s: Polyline) -> Self {
        Shape::Line(s)
    }
}

impl From<Polygon> for Shape {
    fn from(s: Polygon) -> Self {
        Shape::Polygon(s)
    }
}

impl From<Circle> for Shape {
    fn from(s: Circle) -> Self {
        Shape::Circle(s)
    }
}

/// A shape's mesh in its frame.
#[derive(Debug, Clone)]
pub struct ShapeMesh {
    pub drawing: Drawing,
    /// The frame's origin in the world.
    pub origin: WorldPoint,
    /// The zoom whose points the frame measures.
    pub zoom: f64,
    /// The frame's extent in its points.
    pub size: [f32; 2],
    /// Its middle, to pick the world copy nearest the camera.
    pub middle: WorldPoint,
}

/// Places in world units, and their bounds.
fn world(points: &[LngLat], out: &mut Vec<WorldPoint>, lo: &mut WorldPoint, hi: &mut WorldPoint) {
    for p in points {
        let w = p.to_world();
        lo.x = lo.x.min(w.x);
        lo.y = lo.y.min(w.y);
        hi.x = hi.x.max(w.x);
        hi.y = hi.y.max(w.y);
        out.push(w);
    }
}

/// The frame for a shape spanning `lo` to `hi`: its zoom and its scale
/// from world units to its points.
fn frame(lo: WorldPoint, hi: WorldPoint) -> (f64, f64) {
    let side = (hi.x - lo.x).max(hi.y - lo.y).max(1e-12);
    let zoom = (FRAME_SPAN / (side * TILE_SIZE)).log2().clamp(0.0, 24.0);
    (zoom, TILE_SIZE * zoom.exp2())
}

/// No clipping: a square holding every point.
const ALL: Square = Square {
    lo: f32::NEG_INFINITY,
    hi: f32::INFINITY,
};

/// The point `fraction` of the way along `line`, and the index of the
/// segment it lies on.
fn split(line: &[WorldPoint], fraction: f64) -> (usize, WorldPoint) {
    let len: f64 = line
        .windows(2)
        .map(|w| (w[1].x - w[0].x).hypot(w[1].y - w[0].y))
        .sum();
    let mut left = len * fraction;
    for (i, w) in line.windows(2).enumerate() {
        let d = (w[1].x - w[0].x).hypot(w[1].y - w[0].y);
        if left <= d && d > 0.0 {
            let t = left / d;
            return (
                i,
                WorldPoint::new(
                    w[0].x + (w[1].x - w[0].x) * t,
                    w[0].y + (w[1].y - w[0].y) * t,
                ),
            );
        }
        left -= d;
    }
    (line.len().saturating_sub(2), *line.last().expect("points"))
}

/// Builds `shape` with `b`'s scratch; `None` when it draws nothing.
pub fn build_shape(shape: &Shape, b: &mut MeshBuilder) -> Option<ShapeMesh> {
    b.clear();
    let mut lo = WorldPoint::new(f64::INFINITY, f64::INFINITY);
    let mut hi = WorldPoint::new(f64::NEG_INFINITY, f64::NEG_INFINITY);
    let mut pts: Vec<WorldPoint> = Vec::new();
    let mut ends: Vec<usize> = Vec::new();
    let ring;
    let (rings, fill, outline): (&[Vec<LngLat>], Option<Color>, Option<Stroke>) = match shape {
        Shape::Line(l) => {
            world(&l.points, &mut pts, &mut lo, &mut hi);
            ends.push(pts.len());
            (&[], None, None)
        }
        Shape::Polygon(p) => (&p.rings, Some(p.fill), p.outline),
        Shape::Circle(c) => {
            ring = vec![c.ring()];
            (&ring, Some(c.fill), c.outline)
        }
    };
    for r in rings {
        world(r, &mut pts, &mut lo, &mut hi);
        ends.push(pts.len());
    }
    if pts.len() < 2 || !lo.x.is_finite() {
        return None;
    }
    let (zoom, k) = frame(lo, hi);
    let local = |w: WorldPoint| [((w.x - lo.x) * k) as f32, ((w.y - lo.y) * k) as f32];
    let units_per_point = (-24.0f64).exp2() as f32;
    let stroke = |s: Stroke, color: Color, join, cap| LineStyle {
        width: s.width,
        color: color.linear(1.0),
        join,
        cap,
        miter_limit: 2.0,
        dashes: &[],
        units_per_point,
    };
    match shape {
        Shape::Line(l) => {
            let (at, cut) = split(&pts, l.traveled);
            let mut done: Vec<[f32; 2]> = Vec::new();
            let mut rest: Vec<[f32; 2]> = Vec::new();
            if l.traveled > 0.0 {
                done.extend(pts[..=at].iter().map(|&p| local(p)));
                done.push(local(cut));
            }
            if l.traveled < 1.0 {
                rest.push(local(if l.traveled > 0.0 { cut } else { pts[0] }));
                rest.extend(pts[at + 1..].iter().map(|&p| local(p)));
            }
            if let Some(c) = l.casing {
                if done.len() >= 2 {
                    b.line(&done, ALL, &stroke(c, l.traveled_casing, l.join, l.cap));
                }
                if rest.len() >= 2 {
                    b.line(&rest, ALL, &stroke(c, c.color, l.join, l.cap));
                }
            }
            if done.len() >= 2 {
                b.line(
                    &done,
                    ALL,
                    &stroke(l.stroke, l.traveled_color, l.join, l.cap),
                );
            }
            if rest.len() >= 2 {
                b.line(&rest, ALL, &stroke(l.stroke, l.stroke.color, l.join, l.cap));
            }
        }
        Shape::Polygon(_) | Shape::Circle(_) => {
            let mut local_rings: Vec<Vec<[f32; 2]>> = Vec::with_capacity(ends.len());
            let mut start = 0;
            for &end in &ends {
                local_rings.push(pts[start..end].iter().map(|&p| local(p)).collect());
                start = end;
            }
            if let Some(f) = fill {
                b.fill(local_rings.iter().map(Vec::as_slice), ALL, f.linear(1.0));
            }
            if let Some(o) = outline {
                for r in &mut local_rings {
                    if let Some(&first) = r.first() {
                        r.push(first);
                    }
                    b.line(r, ALL, &stroke(o, o.color, Join::Round, Cap::Butt));
                }
            }
        }
    }
    if b.is_empty() {
        return None;
    }
    let size = [
        (((hi.x - lo.x) * k) as f32).max(1.0),
        (((hi.y - lo.y) * k) as f32).max(1.0),
    ];
    let mesh = Mesh::new(b.vertices.as_slice(), b.indices.as_slice());
    Some(ShapeMesh {
        drawing: Drawing::new(vec![Primitive::Mesh(mesh)], Vec2::new(size[0], size[1])),
        origin: lo,
        zoom,
        size,
        middle: WorldPoint::new((lo.x + hi.x) / 2.0, (lo.y + hi.y) / 2.0),
    })
}

/// The user's position: a dot, a wedge toward the heading when known and a
/// circle of the fix's accuracy.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Puck {
    pub at: LngLat,
    /// Degrees clockwise from north the user faces.
    pub heading: Option<f64>,
    /// How far off the fix may be, in meters.
    pub accuracy: f64,
}

impl Puck {
    /// A fix at `at`, good to `accuracy` meters.
    pub const fn new(at: LngLat, accuracy: f64) -> Self {
        Self {
            at,
            heading: None,
            accuracy,
        }
    }

    /// Facing `heading` degrees clockwise from north.
    pub const fn heading(mut self, heading: f64) -> Self {
        self.heading = Some(heading);
        self
    }
}

/// The puck's side, in points; its dot sits in the middle.
pub const PUCK_SIDE: f32 = 48.0;
const PUCK_BLUE: Color = Color::rgb(0x21, 0x73, 0xeb);

/// The puck's dot and, with `heading`, its wedge pointing up, in a square
/// [`PUCK_SIDE`] points across.
pub fn puck_drawing(heading: bool, b: &mut MeshBuilder) -> Drawing {
    b.clear();
    let c = PUCK_SIDE / 2.0;
    if heading {
        let wedge = [[c, c - 21.0], [c + 9.0, c - 3.0], [c - 9.0, c - 3.0]];
        b.fill([&wedge[..]], ALL, PUCK_BLUE.linear(0.55));
    }
    b.circle([c, c], 9.0, Color::WHITE.linear(1.0), None);
    b.circle([c, c], 6.0, PUCK_BLUE.linear(1.0), None);
    let mesh = Mesh::new(b.vertices.as_slice(), b.indices.as_slice());
    Drawing::new(vec![Primitive::Mesh(mesh)], Vec2::new(PUCK_SIDE, PUCK_SIDE))
}

/// The accuracy circle around a fix.
pub fn accuracy_circle(puck: &Puck) -> Circle {
    Circle::new(puck.at, puck.accuracy.max(0.0), PUCK_BLUE.alpha(0.12))
        .outline(Stroke::new(1.0, PUCK_BLUE.alpha(0.35)))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn berlin() -> Vec<LngLat> {
        vec![
            LngLat::new(13.37, 52.51),
            LngLat::new(13.40, 52.52),
            LngLat::new(13.43, 52.50),
        ]
    }

    #[test]
    fn a_shape_builds_in_a_frame_about_a_thousand_points_across() {
        let mut b = MeshBuilder::new();
        let m = build_shape(&Polyline::route(berlin()).into(), &mut b).unwrap();
        assert!(m.size[0] > 500.0 && m.size[0] <= 1024.5, "{:?}", m.size);
        let lo = LngLat::new(13.37, 52.52).to_world();
        assert!((m.origin.x - lo.x).abs() < 1e-12 && (m.origin.y - lo.y).abs() < 1e-12);
        let k = TILE_SIZE * m.zoom.exp2();
        assert!(
            ((LngLat::new(13.43, 0.0).to_world().x - lo.x) * k - f64::from(m.size[0])).abs() < 0.01
        );
    }

    #[test]
    fn a_traveled_route_splits_where_it_was_traveled() {
        let line: Vec<WorldPoint> = [[0.0, 0.0], [1.0, 0.0], [1.0, 3.0]]
            .map(|p| WorldPoint::new(p[0], p[1]))
            .into();
        assert_eq!(split(&line, 0.5), (1, WorldPoint::new(1.0, 1.0)));
        assert_eq!(split(&line, 0.0), (0, WorldPoint::new(0.0, 0.0)));
        assert_eq!(split(&line, 1.0).1, WorldPoint::new(1.0, 3.0));
        let mut b = MeshBuilder::new();
        let whole = build_shape(&Polyline::route(berlin()).into(), &mut b).unwrap();
        let half = build_shape(&Polyline::route(berlin()).traveled(0.5).into(), &mut b).unwrap();
        let count = |m: &ShapeMesh| match &m.drawing.primitives()[0] {
            Primitive::Mesh(m) => m.vertices.len(),
            _ => 0,
        };
        assert!(count(&half) > count(&whole));
    }

    #[test]
    fn a_circle_is_measured_on_the_ground() {
        let c = Circle::new(LngLat::new(13.4, 52.52), 500.0, Color::BLACK);
        for p in c.ring() {
            let d = p.distance(c.center);
            assert!((d - 500.0).abs() < 5.0, "{d}");
        }
        let mut b = MeshBuilder::new();
        assert!(build_shape(&c.clone().into(), &mut b).is_some());
        let none = Polygon::new(vec![LngLat::new(0.0, 0.0)], Color::BLACK);
        assert!(build_shape(&none.into(), &mut b).is_none());
    }
}
