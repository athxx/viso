//! Colour planes: a colour picked by its hue, saturation and brightness, and
//! a swatch showing one.
//!
//! A colour plane is a node's content ([`Content::ColorPlane`]). It holds the
//! colour it shows as [`Hsv`] and an opacity; whoever drives it — the
//! `ColorPicker` widget — keeps the colour and tells the plane
//! ([`NodeStore::show_color`]). It draws one of three shapes:
//!
//! - a [`Square`](PlaneShape::Square): saturation growing to the right and
//!   brightness upward, at the hue it is told;
//! - a [`Wheel`](PlaneShape::Wheel): a ring of hues, red at the top and
//!   running clockwise, round such a square;
//! - a [`Swatch`](PlaneShape::Swatch): the colour, its left half opaque and
//!   its right at its opacity over a checkerboard, when it has any
//!   transparency.
//!
//! The square and the ring draw as meshes whose every vertex carries its
//! exact colour, so what shows matches the numbers picked; a puck, white
//! ringed in a dark hairline, marks the hue on the ring and the tone on the
//! square, and a focus ring rounds the puck of the part with the focus.
//!
//! [`plane_input`] reads what a sample asks of a plane: a press on the ring
//! or the square picks there and a drag carries on with that part, clamped
//! to it; the arrows step the focused part — the tone's saturation across
//! and brightness up and down, the hue round — by a hundredth, or a tenth
//! with Shift.
//!
//! [`rounded_mesh`] draws any rounded rect a colour at a time across it, the
//! colour bars of a slider ([`TrackPaint`](crate::slider::TrackPaint))
//! included.

use std::f32::consts::{PI, TAU};

use viso_math::{linear_to_srgb, srgb_to_linear};
use viso_render::{
    AnalyticEllipse, AnalyticRRect, Border, Corners, Mesh, MeshVertex, Primitive, Rect, Rgba,
};

use crate::component::{BuildCx, Handle, LeafStyle, NodeStore};
use crate::content::Content;
use crate::context::EventCx;
use crate::dirty::DirtyClass;
use crate::input::{Key, PointerButtons, PointerPhase};
use crate::layout::{Size, Vec2};
use crate::node::NodeId;
use crate::style::BoxStyle;

/// A square plane's size unless its node sizes it.
pub const SQUARE_SIZE: Vec2 = Vec2 { x: 200.0, y: 150.0 };
/// A wheel's size unless its node sizes it.
pub const WHEEL_SIZE: Vec2 = Vec2 { x: 200.0, y: 200.0 };
/// A swatch's size unless its node sizes it.
pub const SWATCH_SIZE: Vec2 = Vec2 { x: 28.0, y: 28.0 };

/// A puck's radius.
const PUCK: f32 = 7.0;
/// The rounding of a square's corners.
const SQUARE_RADIUS: f32 = 6.0;
/// The rounding of a wheel's inner square.
const INNER_RADIUS: f32 = 2.0;
/// The ring's outer and inner radii, as fractions of the wheel's side.
const RING_OUTER: f32 = 0.48;
const RING_INNER: f32 = 0.385;
/// Half the inner square's side, as a fraction of the ring's inner radius.
const INNER_HALF: f32 = 0.66;
/// How many cells a tone square is cut into along each side.
const TONE_CELLS: u32 = 16;
/// How many pieces the ring is cut into.
const RING_SEGMENTS: u32 = 96;
/// How many pieces each rounded corner is cut into.
const CORNER_SEGMENTS: u32 = 6;
/// The width of the fringe that smooths a mesh's edge.
const FRINGE: f32 = 0.8;
/// The side of a checkerboard square.
pub const CHECK: f32 = 4.0;
/// A checkerboard's light and dark squares.
const CHECK_LIGHT: Rgba = Rgba::new(1.0, 1.0, 1.0, 1.0);
const CHECK_DARK: Rgba = Rgba::new(0.8, 0.8, 0.8, 1.0);
/// How far an arrow steps, and with Shift held.
const NUDGE: f32 = 0.01;
const NUDGE_COARSE: f32 = 0.1;

/// A colour by its hue, saturation and brightness (value), each `0` to `1`
/// — the hue a turn from red — over gamma-encoded sRGB.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Hsv {
    pub h: f32,
    pub s: f32,
    pub v: f32,
}

impl Hsv {
    pub const fn new(h: f32, s: f32, v: f32) -> Hsv {
        Hsv { h, s, v }
    }

    /// Its gamma-encoded sRGB channels.
    pub fn to_srgb(self) -> [f32; 3] {
        let (s, v) = (clamp01(self.s), clamp01(self.v));
        let h = self.h.rem_euclid(1.0) * 6.0;
        let sector = h.floor();
        let f = h - sector;
        let p = v * (1.0 - s);
        let q = v * (1.0 - s * f);
        let t = v * (1.0 - s * (1.0 - f));
        match sector as u32 {
            0 => [v, t, p],
            1 => [q, v, p],
            2 => [p, v, t],
            3 => [p, q, v],
            4 => [t, p, v],
            _ => [v, p, q],
        }
    }

    /// The hue, saturation and brightness of sRGB channels `rgb`; a grey's
    /// hue is `0`.
    pub fn from_srgb(rgb: [f32; 3]) -> Hsv {
        let [r, g, b] = rgb.map(clamp01);
        let max = r.max(g).max(b);
        let min = r.min(g).min(b);
        let d = max - min;
        let s = if max > 0.0 { d / max } else { 0.0 };
        let h = if d <= 0.0 {
            0.0
        } else if max == r {
            ((g - b) / d).rem_euclid(6.0)
        } else if max == g {
            (b - r) / d + 2.0
        } else {
            (r - g) / d + 4.0
        };
        Hsv::new(h / 6.0, s, max)
    }

    /// It at opacity `alpha`, as the straight linear colour the paint takes.
    pub fn to_rgba(self, alpha: f32) -> Rgba {
        let [r, g, b] = self.to_srgb().map(srgb_to_linear);
        Rgba::new(r, g, b, clamp01(alpha))
    }

    /// The hue, saturation and brightness of straight linear `color`, keeping
    /// `was`'s hue where it has none to tell — a grey, or black — and
    /// `was`'s saturation where black has none either.
    pub fn of(color: Rgba, was: Hsv) -> Hsv {
        let mut hsv = Hsv::from_srgb([color.r, color.g, color.b].map(linear_to_srgb));
        if hsv.s <= 1e-4 || hsv.v <= 1e-4 {
            hsv.h = was.h;
        }
        if hsv.v <= 1e-4 {
            hsv.s = was.s;
        }
        hsv
    }

    /// The colour at full saturation and brightness of its hue.
    fn pure(self) -> Hsv {
        Hsv::new(self.h, 1.0, 1.0)
    }
}

/// `text` read as a hexadecimal sRGB colour — `#rgb`, `#rgba`, `#rrggbb` or
/// `#rrggbbaa`, the `#` optional, either case — as straight linear RGBA.
pub fn parse_hex(text: &str) -> Option<Rgba> {
    let digits = text.trim();
    let digits = digits.strip_prefix('#').unwrap_or(digits);
    if !digits.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    let nibble = |i: usize| u8::from_str_radix(&digits[i..=i], 16).ok();
    let byte = |i: usize| u8::from_str_radix(&digits[i..i + 2], 16).ok();
    let [r, g, b, a] = match digits.len() {
        3 | 4 => {
            let mut c = [0xff_u8; 4];
            for (i, c) in c.iter_mut().enumerate().take(digits.len()) {
                *c = nibble(i)? * 17;
            }
            c
        }
        6 | 8 => {
            let mut c = [0xff_u8; 4];
            for (i, c) in c.iter_mut().enumerate().take(digits.len() / 2) {
                *c = byte(i * 2)?;
            }
            c
        }
        _ => return None,
    };
    let channel = |c: u8| srgb_to_linear(f32::from(c) / 255.0);
    Some(Rgba::new(
        channel(r),
        channel(g),
        channel(b),
        f32::from(a) / 255.0,
    ))
}

/// Straight linear `color` as `#RRGGBB`, or `#RRGGBBAA` when `alpha` asks.
pub fn format_hex(color: Rgba, alpha: bool) -> String {
    let packed = rgba32(color);
    if alpha {
        format!("#{packed:08X}")
    } else {
        format!("#{:06X}", packed >> 8)
    }
}

/// Straight linear `color` as 8-bit sRGB packed `0xRRGGBBAA`, its alpha
/// linear.
pub fn rgba32(color: Rgba) -> u32 {
    let byte = |c: f32| (clamp01(c) * 255.0).round() as u32;
    let [r, g, b] = [color.r, color.g, color.b].map(|c| byte(linear_to_srgb(c)));
    (r << 24) | (g << 16) | (b << 8) | byte(color.a)
}

/// How a colour plane draws.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlaneShape {
    /// Saturation across, brightness up, at a hue.
    Square,
    /// A ring of hues round such a square.
    Wheel,
    /// The colour itself.
    Swatch,
}

/// A part of a plane a drag or the focus is on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlanePart {
    /// A wheel's ring.
    Hue,
    /// The square: saturation and brightness.
    Tone,
}

/// What a colour plane draws.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ColorPlane {
    pub shape: PlaneShape,
    pub hsv: Hsv,
    /// The opacity a swatch shows.
    pub alpha: f32,
    /// The ring a focused part's puck wears.
    pub accent: Rgba,
    /// The part a drag carries, if one does.
    pub drag: Option<PlanePart>,
    /// The part with the focus, if one has it.
    pub focus: Option<PlanePart>,
    /// Its size unless its node sizes it.
    pub natural: Vec2,
}

impl ColorPlane {
    /// A plane of `shape` showing `hsv` at opacity `alpha`, its focused puck
    /// ringed in `accent`.
    pub fn new(shape: PlaneShape, hsv: Hsv, alpha: f32, accent: Rgba) -> ColorPlane {
        let natural = match shape {
            PlaneShape::Square => SQUARE_SIZE,
            PlaneShape::Wheel => WHEEL_SIZE,
            PlaneShape::Swatch => SWATCH_SIZE,
        };
        ColorPlane {
            shape,
            hsv,
            alpha: clamp01(alpha),
            accent,
            drag: None,
            focus: None,
            natural,
        }
    }

    /// Where it lays out in `rect`.
    fn lay(&self, rect: Rect) -> Lay {
        match self.shape {
            PlaneShape::Square | PlaneShape::Swatch => Lay {
                tone: inset(rect, PUCK + 1.0),
                ..Lay::default()
            },
            PlaneShape::Wheel => {
                let side = rect.w.min(rect.h);
                let centre = Vec2::new(rect.x + rect.w * 0.5, rect.y + rect.h * 0.5);
                let inner = side * RING_INNER;
                let half = inner * INNER_HALF;
                Lay {
                    tone: Rect::new(centre.x - half, centre.y - half, half * 2.0, half * 2.0),
                    centre,
                    outer: side * RING_OUTER,
                    inner,
                }
            }
        }
    }

    /// The part of a plane of `rect` a press at window point `at` is on.
    fn part_at(&self, rect: Rect, at: Vec2) -> Option<PlanePart> {
        let lay = self.lay(rect);
        match self.shape {
            PlaneShape::Swatch => None,
            PlaneShape::Square => Some(PlanePart::Tone),
            PlaneShape::Wheel => {
                let d = (at - lay.centre).length();
                if d >= lay.inner - 4.0 && d <= lay.outer + 4.0 {
                    Some(PlanePart::Hue)
                } else if inset(lay.tone, -PUCK).contains(at.x, at.y) {
                    Some(PlanePart::Tone)
                } else {
                    None
                }
            }
        }
    }

    /// What a press or a drag at window point `at` on `part` of a plane of
    /// `rect` picks.
    fn pick(&self, rect: Rect, part: PlanePart, at: Vec2) -> PlaneInput {
        let lay = self.lay(rect);
        match part {
            PlanePart::Hue => {
                let d = at - lay.centre;
                PlaneInput::Hue((d.x.atan2(-d.y) / TAU).rem_euclid(1.0))
            }
            PlanePart::Tone => {
                let t = lay.tone;
                let s = if t.w > 0.0 { (at.x - t.x) / t.w } else { 0.0 };
                let v = if t.h > 0.0 {
                    1.0 - (at.y - t.y) / t.h
                } else {
                    0.0
                };
                PlaneInput::Tone(clamp01(s), clamp01(v))
            }
        }
    }

    /// Draws it into `rect` (world space).
    pub(crate) fn paint(&self, rect: Rect, out: &mut Vec<Primitive>) {
        if rect.w <= 0.0 || rect.h <= 0.0 {
            return;
        }
        match self.shape {
            PlaneShape::Swatch => paint_swatch(rect, self.hsv.to_rgba(self.alpha), out),
            PlaneShape::Square => {
                let lay = self.lay(rect);
                self.paint_tone(lay.tone, SQUARE_RADIUS, out);
            }
            PlaneShape::Wheel => {
                let lay = self.lay(rect);
                out.push(Primitive::Mesh(ring_mesh(lay.centre, lay.outer, lay.inner)));
                self.paint_tone(lay.tone, INNER_RADIUS, out);
                let (sin, cos) = (self.hsv.h * TAU).sin_cos();
                let mid = (lay.outer + lay.inner) * 0.5;
                let at = Vec2::new(lay.centre.x + sin * mid, lay.centre.y - cos * mid);
                let r = ((lay.outer - lay.inner) * 0.5 + 1.0).min(PUCK + 1.0);
                self.puck(at, r, self.hsv.pure(), PlanePart::Hue, out);
            }
        }
    }

    /// The square of tones at this hue in `tone`, its corners rounded by
    /// `radius`, and its puck.
    fn paint_tone(&self, tone: Rect, radius: f32, out: &mut Vec<Primitive>) {
        if tone.w <= 0.0 || tone.h <= 0.0 {
            return;
        }
        let h = self.hsv.h;
        let color = |x: f32, y: f32| Hsv::new(h, x / tone.w, 1.0 - y / tone.h).to_rgba(1.0);
        out.push(Primitive::Mesh(rounded_mesh(
            tone,
            radius,
            [TONE_CELLS, TONE_CELLS],
            Shade::Smooth(&color),
        )));
        let at = Vec2::new(
            tone.x + tone.w * clamp01(self.hsv.s),
            tone.y + tone.h * (1.0 - clamp01(self.hsv.v)),
        );
        self.puck(at, PUCK, self.hsv, PlanePart::Tone, out);
    }

    /// A puck of radius `r` about `at` showing `hsv`, ringed when `part` has
    /// the focus.
    fn puck(&self, at: Vec2, r: f32, hsv: Hsv, part: PlanePart, out: &mut Vec<Primitive>) {
        let disc = |r: f32, color: Rgba, border: Border| {
            Primitive::AnalyticEllipse(AnalyticEllipse {
                rect: Rect::new(at.x - r, at.y - r, r * 2.0, r * 2.0),
                color,
                border,
            })
        };
        if self.focus == Some(part) {
            out.push(disc(
                r + 3.5,
                Rgba::TRANSPARENT,
                Border {
                    width: 2.0,
                    color: self.accent,
                },
            ));
        }
        let r = if self.drag == Some(part) { r + 1.0 } else { r };
        out.push(disc(
            r + 0.75,
            Rgba::TRANSPARENT,
            Border {
                width: 1.0,
                color: Rgba::new(0.0, 0.0, 0.0, 0.35),
            },
        ));
        out.push(disc(
            r,
            hsv.to_rgba(1.0),
            Border {
                width: 2.5,
                color: Rgba::new(1.0, 1.0, 1.0, 1.0),
            },
        ));
    }
}

/// Where a plane lays out in its rect.
#[derive(Debug, Clone, Copy, Default)]
struct Lay {
    /// The square of tones.
    tone: Rect,
    /// A wheel's centre and its ring's radii.
    centre: Vec2,
    outer: f32,
    inner: f32,
}

/// A swatch of `color` filling `rect`: the left half opaque and the right at
/// its opacity over a checkerboard, or all opaque.
fn paint_swatch(rect: Rect, color: Rgba, out: &mut Vec<Primitive>) {
    // Rounded no more than a checkerboard square, so the board's corners
    // fall within the colour's.
    let radius = SQUARE_RADIUS.min(CHECK).min(rect.w * 0.5).min(rect.h * 0.5);
    let hairline = Border {
        width: 1.0,
        color: Rgba::new(0.0, 0.0, 0.0, 0.18),
    };
    let rrect = |rect: Rect, color: Rgba, radius: Corners, border: Border| {
        Primitive::AnalyticRRect(AnalyticRRect {
            rect,
            color,
            radius,
            border,
        })
    };
    if color.a < 1.0 {
        out.push(Primitive::Mesh(checkerboard(rect, radius)));
        out.push(rrect(rect, color, Corners::uniform(radius), Border::NONE));
        // The opaque half overlaps the clear one, leaving no seam between.
        let left = Corners {
            left_top: radius,
            right_top: 0.0,
            right_bottom: 0.0,
            left_bottom: radius,
        };
        let half = Rect::new(rect.x, rect.y, rect.w * 0.5, rect.h);
        out.push(rrect(half, Rgba { a: 1.0, ..color }, left, Border::NONE));
        out.push(rrect(
            rect,
            Rgba::TRANSPARENT,
            Corners::uniform(radius),
            hairline,
        ));
        return;
    }
    out.push(rrect(rect, color, Corners::uniform(radius), hairline));
}

/// A checkerboard filling `rect`, its corners rounded by `radius`, no more
/// than a square's side.
pub(crate) fn checkerboard(rect: Rect, radius: f32) -> Mesh {
    let cols = (rect.w / CHECK).round().max(1.0);
    let rows = (rect.h / CHECK).round().max(1.0);
    let (cw, ch) = (rect.w / cols, rect.h / rows);
    let color = |x: f32, y: f32| {
        if ((x / cw) as u32 + (y / ch) as u32).is_multiple_of(2) {
            CHECK_LIGHT
        } else {
            CHECK_DARK
        }
    };
    rounded_mesh(
        rect,
        radius.min(cw).min(ch),
        [cols as u32, rows as u32],
        Shade::Flat(&color),
    )
}

/// How a [`rounded_mesh`] colours its cells, from a point within its rect
/// (relative to the rect's top left).
#[derive(Clone, Copy)]
pub(crate) enum Shade<'a> {
    /// Each vertex the colour at it, blended across each cell.
    Smooth(&'a dyn Fn(f32, f32) -> Rgba),
    /// Each cell one colour, the colour at its centre.
    Flat(&'a dyn Fn(f32, f32) -> Rgba),
}

/// A mesh filling `rect`, its corners rounded by `radius`, cut into
/// `cells[0]` by `cells[1]` cells between its corners and coloured by
/// `shade`, its edge smoothed by a fringe. A flat shade colours each corner
/// as the cell it falls in when its rounding is within a cell.
pub(crate) fn rounded_mesh(rect: Rect, radius: f32, cells: [u32; 2], shade: Shade<'_>) -> Mesh {
    let r = radius.min(rect.w * 0.5).min(rect.h * 0.5).max(0.5);
    let xs = lines(rect.w, r, cells[0]);
    let ys = lines(rect.h, r, cells[1]);
    let (nx, ny) = (xs.len(), ys.len());
    let mut mesh = MeshBuilder::new(nx * ny + 4 * (nx + ny) * 2);
    let origin = Vec2::new(rect.x, rect.y);
    let local = |p: Vec2| (p.x - origin.x, p.y - origin.y);
    let at = |x: f32, y: f32| Vec2::new(origin.x + x, origin.y + y);
    let colour = |p: Vec2| {
        let (x, y) = local(p);
        match shade {
            Shade::Smooth(f) | Shade::Flat(f) => f(x.clamp(0.0, rect.w), y.clamp(0.0, rect.h)),
        }
    };
    let flat = matches!(shade, Shade::Flat(_));
    // The grid's shared vertices, for a smooth shade.
    let mut grid = vec![u32::MAX; if flat { 0 } else { nx * ny }];
    let mut corner = |mesh: &mut MeshBuilder, i: usize, j: usize| -> u32 {
        let slot = &mut grid[j * nx + i];
        if *slot == u32::MAX {
            let p = at(xs[i], ys[j]);
            *slot = mesh.vertex(p, colour(p), 1.0);
        }
        *slot
    };
    // Each cell's corner, in its quadrant: the arc's centre and the angle it
    // starts from, `None` for a cell no corner rounds.
    let rounds = |i: usize, j: usize| -> Option<(Vec2, f32)> {
        let left = i == 0;
        let right = i == nx - 2;
        let top = j == 0;
        let bottom = j == ny - 2;
        match (left, right, top, bottom) {
            (true, _, true, _) => Some((at(r, r), PI)),
            (_, true, true, _) => Some((at(rect.w - r, r), PI * 1.5)),
            (_, true, _, true) => Some((at(rect.w - r, rect.h - r), 0.0)),
            (true, _, _, true) => Some((at(r, rect.h - r), PI * 0.5)),
            _ => None,
        }
    };
    for j in 0..ny - 1 {
        for i in 0..nx - 1 {
            let centre = at((xs[i] + xs[i + 1]) * 0.5, (ys[j] + ys[j + 1]) * 0.5);
            let flat_colour = colour(centre);
            if let Some((c, from)) = rounds(i, j) {
                // A fan from the arc's centre, then its fringe.
                let arc = |k: u32| {
                    let a = from + PI * 0.5 * k as f32 / CORNER_SEGMENTS as f32;
                    Vec2::new(c.x + a.cos() * r, c.y + a.sin() * r)
                };
                let hub = mesh.vertex(c, if flat { flat_colour } else { colour(c) }, 1.0);
                let mut last = None;
                for k in 0..=CORNER_SEGMENTS {
                    let p = arc(k);
                    let fill = if flat { flat_colour } else { colour(p) };
                    let rim = mesh.vertex(p, fill, 1.0);
                    let n = (p - c) * (1.0 / r.max(1e-3));
                    let out = mesh.vertex(p + n * FRINGE, fill, 0.0);
                    if let Some((prev_rim, prev_out)) = last {
                        mesh.triangle(hub, prev_rim, rim);
                        mesh.quad(prev_rim, rim, out, prev_out);
                    }
                    last = Some((rim, out));
                }
                continue;
            }
            let quad = if flat {
                [(i, j), (i + 1, j), (i + 1, j + 1), (i, j + 1)].map(|(i, j)| {
                    let p = at(xs[i], ys[j]);
                    mesh.vertex(p, flat_colour, 1.0)
                })
            } else {
                [(i, j), (i + 1, j), (i + 1, j + 1), (i, j + 1)]
                    .map(|(i, j)| corner(&mut mesh, i, j))
            };
            mesh.quad(quad[0], quad[1], quad[2], quad[3]);
            // The fringe of each side on the rect's edge.
            let sides = [
                (j == 0, (i, j), (i + 1, j), Vec2::new(0.0, -1.0)),
                (i == nx - 2, (i + 1, j), (i + 1, j + 1), Vec2::new(1.0, 0.0)),
                (j == ny - 2, (i + 1, j + 1), (i, j + 1), Vec2::new(0.0, 1.0)),
                (i == 0, (i, j + 1), (i, j), Vec2::new(-1.0, 0.0)),
            ];
            for (on_edge, a, b, n) in sides {
                if !on_edge {
                    continue;
                }
                let (pa, pb) = (at(xs[a.0], ys[a.1]), at(xs[b.0], ys[b.1]));
                let (ca, cb) = if flat {
                    (flat_colour, flat_colour)
                } else {
                    (colour(pa), colour(pb))
                };
                let va = mesh.vertex(pa, ca, 1.0);
                let vb = mesh.vertex(pb, cb, 1.0);
                let oa = mesh.vertex(pa + n * FRINGE, ca, 0.0);
                let ob = mesh.vertex(pb + n * FRINGE, cb, 0.0);
                mesh.quad(va, vb, ob, oa);
            }
        }
    }
    mesh.finish()
}

/// The cut lines across a length `len` with corners of `r`: its ends, the
/// corners' inner edges, and `cells` cells evenly between.
fn lines(len: f32, r: f32, cells: u32) -> Vec<f32> {
    let cells = cells.max(1);
    let mut out = Vec::with_capacity(cells as usize + 3);
    out.push(0.0);
    let (from, to) = (r, len - r);
    if to - from > 1e-3 {
        for k in 0..=cells {
            out.push(from + (to - from) * k as f32 / cells as f32);
        }
    } else {
        out.push(len * 0.5);
    }
    out.push(len);
    out.dedup_by(|a, b| (*a - *b).abs() < 1e-3);
    out
}

/// A ring of hues about `centre` between radii `inner` and `outer`, red at
/// the top and running clockwise.
fn ring_mesh(centre: Vec2, outer: f32, inner: f32) -> Mesh {
    let n = RING_SEGMENTS;
    let mut mesh = MeshBuilder::new(4 * (n as usize + 1));
    let mut last: Option<[u32; 4]> = None;
    for k in 0..=n {
        let h = k as f32 / n as f32;
        let (sin, cos) = (h * TAU).sin_cos();
        let way = Vec2::new(sin, -cos);
        let fill = Hsv::new(h, 1.0, 1.0).to_rgba(1.0);
        let ring = [
            (inner - FRINGE, 0.0),
            (inner, 1.0),
            (outer, 1.0),
            (outer + FRINGE, 0.0),
        ]
        .map(|(radius, edge)| mesh.vertex(centre + way * radius, fill, edge));
        if let Some(prev) = last {
            for band in 0..3 {
                mesh.quad(prev[band], prev[band + 1], ring[band + 1], ring[band]);
            }
        }
        last = Some(ring);
    }
    mesh.finish()
}

/// A mesh under construction.
struct MeshBuilder {
    vertices: Vec<MeshVertex>,
    indices: Vec<u32>,
}

impl MeshBuilder {
    fn new(vertices: usize) -> MeshBuilder {
        MeshBuilder {
            vertices: Vec::with_capacity(vertices),
            indices: Vec::with_capacity(vertices * 3),
        }
    }

    fn vertex(&mut self, p: Vec2, color: Rgba, edge: f32) -> u32 {
        self.vertices.push(MeshVertex {
            pos: [p.x, p.y],
            color: [color.r, color.g, color.b, color.a],
            edge,
        });
        (self.vertices.len() - 1) as u32
    }

    fn triangle(&mut self, a: u32, b: u32, c: u32) {
        self.indices.extend_from_slice(&[a, b, c]);
    }

    fn quad(&mut self, a: u32, b: u32, c: u32, d: u32) {
        self.indices.extend_from_slice(&[a, b, c, a, c, d]);
    }

    fn finish(self) -> Mesh {
        Mesh {
            vertices: self.vertices,
            indices: self.indices,
        }
    }
}

fn clamp01(f: f32) -> f32 {
    if f.is_nan() { 0.0 } else { f.clamp(0.0, 1.0) }
}

/// `rect` shrunk by `by` on every side, or grown when `by` is negative.
fn inset(rect: Rect, by: f32) -> Rect {
    Rect::new(
        rect.x + by,
        rect.y + by,
        (rect.w - by * 2.0).max(0.0),
        (rect.h - by * 2.0).max(0.0),
    )
}

/// What a sample asks of a colour plane ([`plane_input`]).
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum PlaneInput {
    /// A press or a drag on the ring: this hue.
    Hue(f32),
    /// A press or a drag on the square: this saturation and brightness.
    Tone(f32, f32),
    /// A key steps the hue round by this much.
    StepHue(f32),
    /// A key steps the saturation and the brightness by these.
    StepTone(f32, f32),
    /// The drag let go.
    Release,
}

/// What a handler makes of a plane's bookkeeping, applied by the router
/// ([`EventCx::change_plane`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlaneChange {
    /// A drag carries this part, or none.
    Drag(Option<PlanePart>),
    /// This part has the focus, or none.
    Focus(Option<PlanePart>),
}

/// What a sample on colour plane `node` asks of it — the keys acting on
/// `focus`, its focused part — recording the drag it starts or ends; `None`
/// for one that asks nothing. The arrows stop propagating.
pub fn plane_input(ev: &mut EventCx<'_>, node: NodeId, focus: PlanePart) -> Option<PlaneInput> {
    let plane = ev.color_plane(node)?;
    if let Some(p) = ev.pointer().copied() {
        let rect = ev.rect_of(node)?;
        let at = Vec2::new(p.x, p.y);
        let primary = p.buttons.contains(PointerButtons::PRIMARY);
        return match p.phase {
            PointerPhase::Down if primary => {
                let part = plane.part_at(rect, at)?;
                ev.capture_pointer(node);
                ev.change_plane(node, PlaneChange::Drag(Some(part)));
                Some(plane.pick(rect, part, at))
            }
            PointerPhase::Move if primary => {
                let part = plane.drag?;
                Some(plane.pick(rect, part, at))
            }
            PointerPhase::Up if plane.drag.is_some() => {
                ev.change_plane(node, PlaneChange::Drag(None));
                ev.release_pointer();
                Some(PlaneInput::Release)
            }
            _ => None,
        };
    }
    let k = ev.key().copied().filter(|k| k.pressed)?;
    let step = if k.modifiers.shift {
        NUDGE_COARSE
    } else {
        NUDGE
    };
    let rtl = ev.reads_rtl();
    let across = if rtl { -step } else { step };
    let input = match (focus, k.key) {
        (PlanePart::Tone, Key::Right) => PlaneInput::StepTone(across, 0.0),
        (PlanePart::Tone, Key::Left) => PlaneInput::StepTone(-across, 0.0),
        (PlanePart::Tone, Key::Up) => PlaneInput::StepTone(0.0, step),
        (PlanePart::Tone, Key::Down) => PlaneInput::StepTone(0.0, -step),
        (PlanePart::Hue, Key::Right | Key::Up) => PlaneInput::StepHue(step),
        (PlanePart::Hue, Key::Left | Key::Down) => PlaneInput::StepHue(-step),
        _ => return None,
    };
    ev.stop_propagation();
    Some(input)
}

impl BuildCx<'_> {
    /// A colour plane leaf asking `size` of its parent; see
    /// [`NodeStore::set_color_plane`].
    pub fn color_plane_leaf(&mut self, size: Size, plane: ColorPlane) -> Handle {
        let handle = self.leaf(LeafStyle {
            size,
            style: BoxStyle::NONE,
        });
        self.store.set_color_plane(handle.id(), plane);
        handle
    }
}

impl NodeStore {
    /// Node `id`'s colour plane, if it draws one.
    pub fn color_plane(&self, id: NodeId) -> Option<&ColorPlane> {
        match self.content_payload(id) {
            Some(Content::ColorPlane(plane)) => Some(plane),
            _ => None,
        }
    }

    fn color_plane_mut(&mut self, id: NodeId) -> Option<&mut ColorPlane> {
        if !self.arena().is_live(id) {
            return None;
        }
        match self.content_payload_mut(id) {
            Some(Content::ColorPlane(plane)) => Some(plane),
            _ => None,
        }
    }

    /// Make node `id` draw `plane`; one drawing a plane already keeps its
    /// drag and focus.
    pub fn set_color_plane(&mut self, id: NodeId, plane: ColorPlane) {
        if !self.arena().is_live(id) {
            return;
        }
        let next = match self.color_plane(id) {
            Some(was) => ColorPlane {
                drag: was.drag,
                focus: was.focus,
                ..plane
            },
            None => plane,
        };
        if self.color_plane(id) == Some(&next) {
            return;
        }
        if self.color_plane(id).map(|p| p.natural) != Some(next.natural) {
            self.mark_dirty(id, DirtyClass::MEASURE);
        }
        self.set_content_payload(id, Content::ColorPlane(next));
        self.mark_dirty(id, DirtyClass::PAINT);
    }

    /// Tell colour plane `id` the colour it shows: `hsv` at opacity `alpha`.
    pub fn show_color(&mut self, id: NodeId, hsv: Hsv, alpha: f32) {
        let Some(plane) = self.color_plane_mut(id) else {
            return;
        };
        let alpha = clamp01(alpha);
        if plane.hsv == hsv && plane.alpha == alpha {
            return;
        }
        plane.hsv = hsv;
        plane.alpha = alpha;
        self.mark_dirty(id, DirtyClass::PAINT);
    }

    /// Apply what a handler made of colour plane `id`'s drag or focus.
    pub(crate) fn change_plane(&mut self, id: NodeId, change: PlaneChange) {
        let Some(plane) = self.color_plane_mut(id) else {
            return;
        };
        let was = (plane.drag, plane.focus);
        match change {
            PlaneChange::Drag(part) => plane.drag = part,
            PlaneChange::Focus(part) => plane.focus = part,
        }
        if (plane.drag, plane.focus) != was {
            self.mark_dirty(id, DirtyClass::PAINT);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn near(a: f32, b: f32) -> bool {
        (a - b).abs() < 1e-4
    }

    #[test]
    fn hsv_runs_round_srgb_and_back() {
        assert_eq!(Hsv::new(0.0, 1.0, 1.0).to_srgb(), [1.0, 0.0, 0.0]);
        assert_eq!(Hsv::new(1.0 / 3.0, 1.0, 1.0).to_srgb(), [0.0, 1.0, 0.0]);
        assert_eq!(Hsv::new(0.5, 0.0, 0.5).to_srgb(), [0.5, 0.5, 0.5]);
        for &(h, s, v) in &[(0.1, 0.4, 0.9), (0.55, 1.0, 0.3), (0.9, 0.2, 0.6)] {
            let back = Hsv::from_srgb(Hsv::new(h, s, v).to_srgb());
            assert!(
                near(back.h, h) && near(back.s, s) && near(back.v, v),
                "{back:?}"
            );
        }
        let grey = Hsv::of(Rgba::new(0.2, 0.2, 0.2, 1.0), Hsv::new(0.6, 0.8, 0.1));
        assert!(
            near(grey.h, 0.6) && near(grey.s, 0.0),
            "a grey keeps the hue"
        );
        let black = Hsv::of(Rgba::new(0.0, 0.0, 0.0, 1.0), Hsv::new(0.6, 0.8, 0.1));
        assert_eq!((black.h, black.s, black.v), (0.6, 0.8, 0.0));
    }

    #[test]
    fn hex_reads_its_four_forms_and_writes_its_two() {
        let red = parse_hex("#ff0000").unwrap();
        assert_eq!((red.r, red.g, red.b, red.a), (1.0, 0.0, 0.0, 1.0));
        assert_eq!(parse_hex("f00"), Some(red));
        assert_eq!(parse_hex(" #F00F "), Some(red));
        let half = parse_hex("#00000080").unwrap();
        assert!(near(half.a, 128.0 / 255.0));
        assert_eq!(parse_hex("#12345"), None);
        assert_eq!(parse_hex("#ggg"), None);
        assert_eq!(parse_hex(""), None);
        let mid = parse_hex("#3a7bd5").unwrap();
        assert_eq!(format_hex(mid, false), "#3A7BD5");
        assert_eq!(format_hex(half, true), "#00000080");
    }

    #[test]
    fn a_wheel_picks_its_ring_round_from_the_top_and_its_square_within() {
        let wheel = ColorPlane::new(PlaneShape::Wheel, Hsv::default(), 1.0, Rgba::TRANSPARENT);
        let rect = Rect::new(0.0, 0.0, 200.0, 200.0);
        let top = Vec2::new(100.0, 100.0 - 86.0);
        assert_eq!(wheel.part_at(rect, top), Some(PlanePart::Hue));
        assert_eq!(wheel.pick(rect, PlanePart::Hue, top), PlaneInput::Hue(0.0));
        let right = Vec2::new(186.0, 100.0);
        assert_eq!(
            wheel.pick(rect, PlanePart::Hue, right),
            PlaneInput::Hue(0.25)
        );
        assert_eq!(
            wheel.part_at(rect, Vec2::new(100.0, 100.0)),
            Some(PlanePart::Tone)
        );
        assert_eq!(
            wheel.pick(rect, PlanePart::Tone, Vec2::new(100.0, 100.0)),
            PlaneInput::Tone(0.5, 0.5)
        );
        assert_eq!(wheel.part_at(rect, Vec2::new(2.0, 2.0)), None);
        let square = ColorPlane::new(PlaneShape::Square, Hsv::default(), 1.0, Rgba::TRANSPARENT);
        assert_eq!(
            square.pick(rect, PlanePart::Tone, Vec2::new(-50.0, 300.0)),
            PlaneInput::Tone(0.0, 0.0),
            "a drag beyond the square holds to it"
        );
    }

    #[test]
    fn a_rounded_mesh_colours_its_vertices_and_fringes_its_edge() {
        let rect = Rect::new(10.0, 20.0, 40.0, 30.0);
        let f = |x: f32, _y: f32| Rgba::new(x / 40.0, 0.0, 0.0, 1.0);
        let mesh = rounded_mesh(rect, 4.0, [4, 3], Shade::Smooth(&f));
        assert_eq!(mesh.indices.len() % 3, 0);
        assert!(
            mesh.indices
                .iter()
                .all(|&i| (i as usize) < mesh.vertices.len())
        );
        for v in &mesh.vertices {
            let [x, y] = v.pos;
            assert!(x >= rect.x - FRINGE - 1e-3 && x <= rect.x + rect.w + FRINGE + 1e-3);
            assert!(y >= rect.y - FRINGE - 1e-3 && y <= rect.y + rect.h + FRINGE + 1e-3);
            if v.edge == 1.0 {
                assert!(near(v.color[0], (x - rect.x) / 40.0), "{v:?}");
            }
        }
        // No interior vertex sits beyond a rounded corner.
        for v in mesh.vertices.iter().filter(|v| v.edge == 1.0) {
            let [x, y] = [v.pos[0] - rect.x, v.pos[1] - rect.y];
            if x < 4.0 && y < 4.0 {
                assert!(((x - 4.0).powi(2) + (y - 4.0).powi(2)).sqrt() <= 4.0 + 1e-3);
            }
        }
        let board = checkerboard(Rect::new(0.0, 0.0, 16.0, 8.0), 4.0);
        let colours: std::collections::BTreeSet<u32> = board
            .vertices
            .iter()
            .map(|v| v.color[1].to_bits())
            .collect();
        assert_eq!(colours.len(), 2, "a checkerboard holds its two greys");
    }
}
