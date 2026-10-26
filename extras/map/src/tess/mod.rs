//! Tessellation into one triangle mesh per tile.
//!
//! Positions are in tile units; everything measured on screen — line
//! widths, joins and caps, circle radii, antialiasing fringes — is a
//! vertex's *extrusion*, an offset in screen points the renderer turns with
//! the map but never scales. So a tile built once draws its lines at the
//! same width across a whole zoom level, and its edges stay one point soft
//! however far the level is magnified.

pub mod clip;
pub mod earcut;

use viso_render::MeshVertex;

use self::clip::Square;
use self::earcut::Earcut;

/// The width of the antialiasing fringe, in points.
pub const AA: f32 = 1.0;

/// How consecutive line segments meet.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Join {
    /// Sharp corners, cut flat past the miter limit.
    Miter,
    /// Corners cut flat.
    Bevel,
    /// Rounded corners.
    #[default]
    Round,
}

/// How a line ends.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Cap {
    /// Flat, at the end point.
    #[default]
    Butt,
    /// Rounded about the end point.
    Round,
    /// Flat, half the width past the end point.
    Square,
}

/// How to stroke a line.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct LineStyle<'a> {
    /// The line's width, in points.
    pub width: f32,
    /// Straight linear RGBA.
    pub color: [f32; 4],
    pub join: Join,
    pub cap: Cap,
    /// The miter join's limit, as a multiple of half the width.
    pub miter_limit: f32,
    /// Dash and gap lengths in multiples of the width, alternating, dash
    /// first; empty draws the line solid.
    pub dashes: &'a [f32],
    /// Tile units per point of screen, for lengths along the line.
    pub units_per_point: f32,
}

/// A mesh being built, and the scratch its builders reuse.
#[derive(Default)]
pub struct MeshBuilder {
    pub vertices: Vec<MeshVertex>,
    pub indices: Vec<u32>,
    earcut: Earcut,
    points: Vec<[f32; 2]>,
    holes: Vec<u32>,
    ring: Vec<[f32; 2]>,
    scratch: Vec<[f32; 2]>,
    line: Vec<[f32; 2]>,
}

#[inline]
fn vertex(pos: [f32; 2], color: [f32; 4], edge: f32, extrude: [f32; 2]) -> MeshVertex {
    MeshVertex {
        extrude,
        ..MeshVertex::new(pos, color, edge)
    }
}

#[inline]
fn normalize(v: [f32; 2]) -> Option<[f32; 2]> {
    let len = v[0].hypot(v[1]);
    (len > 1e-12).then(|| [v[0] / len, v[1] / len])
}

#[inline]
fn scale(v: [f32; 2], k: f32) -> [f32; 2] {
    [v[0] * k, v[1] * k]
}

#[inline]
fn add(a: [f32; 2], b: [f32; 2]) -> [f32; 2] {
    [a[0] + b[0], a[1] + b[1]]
}

/// The arc steps a round join or cap of radius `r` points needs for its
/// chords to stray under a quarter point, over `angle` radians.
fn arc_steps(r: f32, angle: f32) -> u32 {
    if r <= 0.5 {
        return 1;
    }
    let step = 2.0 * (1.0 - 0.25 / r).clamp(-1.0, 1.0).acos();
    ((angle.abs() / step.max(0.05)).ceil() as u32).clamp(1, 64)
}

impl MeshBuilder {
    /// An empty builder.
    pub fn new() -> Self {
        Self::default()
    }

    /// Empty the mesh, keeping its storage.
    pub fn clear(&mut self) {
        self.vertices.clear();
        self.indices.clear();
    }

    /// Whether nothing was built.
    pub fn is_empty(&self) -> bool {
        self.indices.is_empty()
    }

    fn push(&mut self, v: MeshVertex) -> u32 {
        self.vertices.push(v);
        self.vertices.len() as u32 - 1
    }

    fn tri(&mut self, a: u32, b: u32, c: u32) {
        self.indices.extend([a, b, c]);
    }

    fn quad(&mut self, a0: u32, a1: u32, b0: u32, b1: u32) {
        self.indices.extend([a0, b0, b1, a0, b1, a1]);
    }

    /// Fill the polygon whose rings are `rings` — the outer first, then its
    /// holes — clipped to `square`, with an antialiased edge everywhere but
    /// along the square's sides, where the next tile carries on.
    pub fn fill<'r>(
        &mut self,
        rings: impl IntoIterator<Item = &'r [[f32; 2]]>,
        square: Square,
        color: [f32; 4],
    ) {
        self.points.clear();
        self.holes.clear();
        let mut ring = std::mem::take(&mut self.ring);
        let mut scratch = std::mem::take(&mut self.scratch);
        for (k, r) in rings.into_iter().enumerate() {
            clip_ring_dedup(r, square, &mut ring, &mut scratch);
            if ring.len() < 3 {
                if k == 0 {
                    break;
                }
                continue;
            }
            if k > 0 {
                self.holes.push(self.points.len() as u32);
            }
            self.points.extend_from_slice(&ring);
        }
        self.ring = ring;
        self.scratch = scratch;
        if self.points.len() < 3 {
            return;
        }
        let base = self.vertices.len() as u32;
        for &p in &self.points {
            self.vertices.push(vertex(p, color, 1.0, [0.0, 0.0]));
        }
        let start = self.indices.len();
        self.earcut
            .triangulate(&self.points, &self.holes, base, &mut self.indices);
        if self.indices.len() == start {
            self.vertices.truncate(base as usize);
            return;
        }
        // The fringe: each ring's points pushed out along the miter of the
        // edges that do not lie on the square.
        let n = self.points.len();
        let mut at = 0;
        let ends: Vec<usize> = self.holes.iter().map(|&h| h as usize).chain([n]).collect();
        for end in ends {
            self.fringe_ring(base, at, end, square, color);
            at = end;
        }
    }

    fn fringe_ring(
        &mut self,
        base: u32,
        start: usize,
        end: usize,
        square: Square,
        color: [f32; 4],
    ) {
        let n = end - start;
        let pts = &self.points[start..end];
        // Twice the signed area: positive winds clockwise on screen.
        let area: f32 = (0..n)
            .map(|i| {
                let (a, b) = (pts[i], pts[(i + 1) % n]);
                a[0] * b[1] - b[0] * a[1]
            })
            .sum();
        // Outward is left of the direction of travel for a clockwise ring:
        // exterior rings wind clockwise and holes the other way, and both
        // keep the filled side on their right.
        let first_is_outer = start == 0;
        let flip = if first_is_outer {
            area < 0.0
        } else {
            area > 0.0
        };
        let outward = |a: [f32; 2], b: [f32; 2]| {
            let d = normalize([b[0] - a[0], b[1] - a[1]])?;
            Some(if flip { [-d[1], d[0]] } else { [d[1], -d[0]] })
        };
        let fringe_start = self.vertices.len() as u32;
        for i in 0..n {
            let prev = pts[(i + n - 1) % n];
            let p = pts[i];
            let next = pts[(i + 1) % n];
            let a = (!square.on_side(prev, p))
                .then(|| outward(prev, p))
                .flatten();
            let b = (!square.on_side(p, next))
                .then(|| outward(p, next))
                .flatten();
            let e = match (a, b) {
                (Some(a), Some(b)) => match normalize(add(a, b)) {
                    Some(m) => {
                        let cos = (m[0] * b[0] + m[1] * b[1]).max(0.5);
                        scale(m, AA / cos)
                    }
                    None => scale(b, AA),
                },
                (Some(n), None) | (None, Some(n)) => scale(n, AA),
                (None, None) => [0.0, 0.0],
            };
            let v = vertex(p, color, 0.0, e);
            self.vertices.push(v);
        }
        for i in 0..n {
            let j = (i + 1) % n;
            if square.on_side(self.points[start + i], self.points[start + j]) {
                continue;
            }
            let (a0, b0) = (base + (start + i) as u32, base + (start + j) as u32);
            let (a1, b1) = (fringe_start + i as u32, fringe_start + j as u32);
            self.quad(a0, a1, b0, b1);
        }
    }

    /// Stroke the polyline `line` (tile units), clipped to `square`.
    pub fn line(&mut self, line: &[[f32; 2]], square: Square, style: &LineStyle<'_>) {
        if style.width <= 0.0 || style.color[3] <= 0.0 {
            return;
        }
        let mut pts = std::mem::take(&mut self.line);
        pts.clear();
        for &p in line {
            if pts.last() != Some(&p) {
                pts.push(p);
            }
        }
        let mut scratch = std::mem::take(&mut self.scratch);
        let mut runs: Vec<Vec<[f32; 2]>> = Vec::new();
        clip::clip_line(&pts, square, &mut scratch, |run| runs.push(run.to_vec()));
        self.scratch = scratch;
        self.line = pts;
        for run in &runs {
            if style.dashes.len() >= 2 {
                self.dashed(run, style);
            } else {
                self.stroke(run, style, style.cap, style.cap);
            }
        }
    }

    /// Stroke `pts` in dashes.
    fn dashed(&mut self, pts: &[[f32; 2]], style: &LineStyle<'_>) {
        let unit = style.width.max(1.0) * style.units_per_point;
        let pattern: Vec<f32> = style.dashes.iter().map(|d| d.max(0.0) * unit).collect();
        let period: f32 = pattern.iter().sum();
        if period <= 1e-6 {
            return self.stroke(pts, style, style.cap, style.cap);
        }
        let mut k = 0;
        let mut left = pattern[0];
        let mut on = true;
        let mut piece: Vec<[f32; 2]> = vec![pts[0]];
        for w in pts.windows(2) {
            let (mut a, b) = (w[0], w[1]);
            let mut len = (b[0] - a[0]).hypot(b[1] - a[1]);
            while len > left {
                let t = left / len;
                let p = [a[0] + (b[0] - a[0]) * t, a[1] + (b[1] - a[1]) * t];
                if on {
                    piece.push(p);
                    self.stroke(&piece, style, style.cap, style.cap);
                }
                piece.clear();
                piece.push(p);
                len -= left;
                a = p;
                k = (k + 1) % pattern.len();
                left = pattern[k];
                on = !on;
            }
            left -= len;
            if on {
                piece.push(b);
            } else {
                piece.clear();
                piece.push(b);
            }
        }
        if on && piece.len() >= 2 {
            self.stroke(&piece, style, style.cap, style.cap);
        }
    }

    /// A strip station at `p`: the outer and core vertices on both sides of
    /// unit normal `n`, scaled by `k` (a miter's length), indices
    /// `[left outer, left core, right core, right outer]`.
    fn station(
        &mut self,
        p: [f32; 2],
        n: [f32; 2],
        k: f32,
        half: (f32, f32),
        color: [f32; 4],
    ) -> [u32; 4] {
        let (core, outer) = half;
        let lo = self.push(vertex(p, color, 0.0, scale(n, outer * k)));
        let lc = self.push(vertex(p, color, 1.0, scale(n, core * k)));
        let rc = self.push(vertex(p, color, 1.0, scale(n, -core * k)));
        let ro = self.push(vertex(p, color, 0.0, scale(n, -outer * k)));
        [lo, lc, rc, ro]
    }

    fn join_strip(&mut self, a: [u32; 4], b: [u32; 4]) {
        self.quad(a[0], a[1], b[0], b[1]);
        self.quad(a[1], a[2], b[1], b[2]);
        self.quad(a[2], a[3], b[2], b[3]);
    }

    /// A fan about `p` from unit direction `from`, turning by `angle`
    /// radians, in core and fringe rings.
    fn fan(&mut self, p: [f32; 2], from: [f32; 2], angle: f32, half: (f32, f32), color: [f32; 4]) {
        let (core, outer) = half;
        let steps = arc_steps(outer, angle);
        let center = self.push(vertex(p, color, 1.0, [0.0, 0.0]));
        let mut prev = None;
        for s in 0..=steps {
            let t = angle * s as f32 / steps as f32;
            let (sin, cos) = t.sin_cos();
            let d = [from[0] * cos - from[1] * sin, from[0] * sin + from[1] * cos];
            let c = self.push(vertex(p, color, 1.0, scale(d, core)));
            let o = self.push(vertex(p, color, 0.0, scale(d, outer)));
            if let Some((pc, po)) = prev {
                self.tri(center, pc, c);
                self.quad(pc, po, c, o);
            }
            prev = Some((c, o));
        }
    }

    /// A cap at `p` where the line runs along `d` (pointing away from the
    /// line) with left normal `n`, joined to station `s`.
    #[allow(clippy::too_many_arguments)]
    fn cap(
        &mut self,
        p: [f32; 2],
        d: [f32; 2],
        n: [f32; 2],
        s: [u32; 4],
        cap: Cap,
        half: (f32, f32),
        color: [f32; 4],
    ) {
        let (core, outer) = half;
        match cap {
            Cap::Round => {
                // From the left normal round through `d` to the right.
                let cross = n[0] * d[1] - n[1] * d[0];
                let angle = if cross >= 0.0 {
                    std::f32::consts::PI
                } else {
                    -std::f32::consts::PI
                };
                self.fan(p, n, angle, half, color);
            }
            Cap::Butt | Cap::Square => {
                let reach = if cap == Cap::Square {
                    outer - AA / 2.0
                } else {
                    0.0
                };
                let ends = if reach > 0.0 {
                    let push = scale(d, reach);
                    let lo = self.push(vertex(p, color, 0.0, add(scale(n, outer), push)));
                    let lc = self.push(vertex(p, color, 1.0, add(scale(n, core), push)));
                    let rc = self.push(vertex(p, color, 1.0, add(scale(n, -core), push)));
                    let ro = self.push(vertex(p, color, 0.0, add(scale(n, -outer), push)));
                    let e = [lo, lc, rc, ro];
                    self.join_strip(s, e);
                    e
                } else {
                    s
                };
                // The end's own fringe, a point past it.
                let push = scale(d, reach + AA);
                let f: [u32; 4] = [
                    self.push(vertex(p, color, 0.0, add(scale(n, outer), push))),
                    self.push(vertex(p, color, 0.0, add(scale(n, core), push))),
                    self.push(vertex(p, color, 0.0, add(scale(n, -core), push))),
                    self.push(vertex(p, color, 0.0, add(scale(n, -outer), push))),
                ];
                self.join_strip(ends, f);
            }
        }
    }

    /// Stroke the deduplicated polyline `pts`.
    fn stroke(&mut self, pts: &[[f32; 2]], style: &LineStyle<'_>, start_cap: Cap, end_cap: Cap) {
        if pts.len() < 2 {
            return;
        }
        // A hairline thinner than the fringe draws one fringe wide, fainter.
        let mut color = style.color;
        let half_width = style.width / 2.0;
        let half = if style.width >= AA {
            (half_width - AA / 2.0, half_width + AA / 2.0)
        } else {
            color[3] *= style.width / AA;
            (0.0, AA)
        };
        let dirs: Vec<[f32; 2]> = pts
            .windows(2)
            .filter_map(|w| normalize([w[1][0] - w[0][0], w[1][1] - w[0][1]]))
            .collect();
        if dirs.len() + 1 != pts.len() {
            return;
        }
        let left = |d: [f32; 2]| [-d[1], d[0]];
        let n0 = left(dirs[0]);
        let mut s = self.station(pts[0], n0, 1.0, half, color);
        let d0 = dirs[0];
        self.cap(pts[0], [-d0[0], -d0[1]], n0, s, start_cap, half, color);
        let miter_limit = match style.join {
            Join::Miter => style.miter_limit.max(1.0),
            Join::Bevel | Join::Round => 1.05,
        };
        for i in 1..pts.len() - 1 {
            let (da, db) = (dirs[i - 1], dirs[i]);
            let (na, nb) = (left(da), left(db));
            let p = pts[i];
            let m = normalize(add(na, nb));
            let k = m.map(|m| 1.0 / (m[0] * nb[0] + m[1] * nb[1]).max(1e-6));
            match (m, k) {
                (Some(m), Some(k)) if k <= miter_limit => {
                    let next = self.station(p, m, k, half, color);
                    self.join_strip(s, next);
                    s = next;
                }
                _ => {
                    let end = self.station(p, na, 1.0, half, color);
                    self.join_strip(s, end);
                    let start = self.station(p, nb, 1.0, half, color);
                    // The outer side is the one the line turns away from.
                    let cross = da[0] * db[1] - da[1] * db[0];
                    let (from, to) = if cross > 0.0 {
                        ([-na[0], -na[1]], [-nb[0], -nb[1]])
                    } else {
                        (na, nb)
                    };
                    if style.join == Join::Round {
                        let angle = (from[0] * to[1] - from[1] * to[0])
                            .atan2(from[0] * to[0] + from[1] * to[1]);
                        self.fan(p, from, angle, half, color);
                    } else {
                        // A bevel: the triangle across the outer corner and its fringe.
                        let (core, outer) = half;
                        let c = self.push(vertex(p, color, 1.0, [0.0, 0.0]));
                        let ac = self.push(vertex(p, color, 1.0, scale(from, core)));
                        let ao = self.push(vertex(p, color, 0.0, scale(from, outer)));
                        let bc = self.push(vertex(p, color, 1.0, scale(to, core)));
                        let bo = self.push(vertex(p, color, 0.0, scale(to, outer)));
                        self.tri(c, ac, bc);
                        self.quad(ac, ao, bc, bo);
                    }
                    s = start;
                }
            }
        }
        let last = pts.len() - 1;
        let dl = dirs[last - 1];
        let nl = left(dl);
        let end = self.station(pts[last], nl, 1.0, half, color);
        self.join_strip(s, end);
        self.cap(pts[last], dl, nl, end, end_cap, half, color);
    }

    /// A disc of `radius` points about `p`, with a `stroke` of
    /// `(width, color)` outside it.
    pub fn circle(
        &mut self,
        p: [f32; 2],
        radius: f32,
        color: [f32; 4],
        stroke: Option<(f32, [f32; 4])>,
    ) {
        let tau = std::f32::consts::TAU;
        if radius > 0.0 && color[3] > 0.0 {
            let half = ((radius - AA / 2.0).max(0.0), radius + AA / 2.0);
            self.fan(p, [1.0, 0.0], tau, half, color);
        }
        if let Some((width, stroke_color)) = stroke
            && width > 0.0
            && stroke_color[3] > 0.0
        {
            let (inner, outer) = (radius, radius + width);
            let steps = arc_steps(outer, tau);
            let mut prev: Option<[u32; 4]> = None;
            for s in 0..=steps {
                let (sin, cos) = (tau * s as f32 / steps as f32).sin_cos();
                let d = [cos, sin];
                let ring = [
                    self.push(vertex(
                        p,
                        stroke_color,
                        0.0,
                        scale(d, (inner - AA / 2.0).max(0.0)),
                    )),
                    self.push(vertex(p, stroke_color, 1.0, scale(d, inner + AA / 2.0))),
                    self.push(vertex(
                        p,
                        stroke_color,
                        1.0,
                        scale(d, (outer - AA / 2.0).max(inner + AA / 2.0)),
                    )),
                    self.push(vertex(p, stroke_color, 0.0, scale(d, outer + AA / 2.0))),
                ];
                if let Some(prev) = prev {
                    self.join_strip(prev, ring);
                }
                prev = Some(ring);
            }
        }
    }
}

/// Clip `ring` into `out`, dropping repeated points (and a closing point
/// equal to the first).
fn clip_ring_dedup(
    ring: &[[f32; 2]],
    square: Square,
    out: &mut Vec<[f32; 2]>,
    scratch: &mut Vec<[f32; 2]>,
) {
    clip::clip_ring(ring, square, out, scratch);
    out.dedup();
    while out.len() > 1 && out.first() == out.last() {
        out.pop();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SQ: Square = Square {
        lo: 0.0,
        hi: 4096.0,
    };
    const INK: [f32; 4] = [0.0, 0.0, 0.0, 1.0];

    fn style(width: f32) -> LineStyle<'static> {
        LineStyle {
            width,
            color: INK,
            join: Join::Round,
            cap: Cap::Butt,
            miter_limit: 2.0,
            dashes: &[],
            units_per_point: 8.0,
        }
    }

    fn valid(m: &MeshBuilder) {
        assert_eq!(m.indices.len() % 3, 0);
        assert!(m.indices.iter().all(|&i| (i as usize) < m.vertices.len()));
    }

    #[test]
    fn fills_fringe_everywhere_but_the_square() {
        let mut m = MeshBuilder::new();
        let outer = [
            [-100.0, 100.0],
            [2000.0, 100.0],
            [2000.0, 2000.0],
            [-100.0, 2000.0],
        ];
        m.fill([&outer[..]], SQ, INK);
        valid(&m);
        // Four ring points and four fringe points; the side on x = 0 has no
        // fringe quad, the other three do.
        assert_eq!(m.vertices.len(), 8);
        assert_eq!(m.indices.len(), 6 + 3 * 6);
        let fringe = &m.vertices[4..];
        assert!(fringe.iter().all(|v| v.edge == 0.0));
        // A point on the cut side only pushes out along its other edge.
        let on_cut = fringe.iter().find(|v| v.pos == [0.0, 100.0]).unwrap();
        assert_eq!(on_cut.extrude, [0.0, -AA]);
        // Interior points keep no extrusion.
        assert!(
            m.vertices[..4]
                .iter()
                .all(|v| v.extrude == [0.0, 0.0] && v.edge == 1.0)
        );
    }

    #[test]
    fn holes_fringe_into_the_hole() {
        let mut m = MeshBuilder::new();
        let outer = [
            [100.0, 100.0],
            [900.0, 100.0],
            [900.0, 900.0],
            [100.0, 900.0],
        ];
        let hole = [
            [400.0, 400.0],
            [400.0, 600.0],
            [600.0, 600.0],
            [600.0, 400.0],
        ];
        m.fill([&outer[..], &hole[..]], SQ, INK);
        valid(&m);
        let at = |pos: [f32; 2]| {
            m.vertices
                .iter()
                .find(|v| v.pos == pos && v.edge == 0.0)
                .unwrap()
                .extrude
        };
        let e = at([100.0, 100.0]);
        assert!(
            e[0] < 0.0 && e[1] < 0.0,
            "the outer corner pushes out: {e:?}"
        );
        let e = at([400.0, 400.0]);
        assert!(
            e[0] > 0.0 && e[1] > 0.0,
            "the hole's corner pushes into it: {e:?}"
        );
    }

    #[test]
    fn lines_extrude_by_half_their_width() {
        let mut m = MeshBuilder::new();
        m.line(&[[100.0, 100.0], [1000.0, 100.0]], SQ, &style(4.0));
        valid(&m);
        let max = m
            .vertices
            .iter()
            .map(|v| v.extrude[1].abs())
            .fold(0.0, f32::max);
        assert_eq!(max, 2.0 + AA / 2.0);
        let core = m
            .vertices
            .iter()
            .filter(|v| v.edge == 1.0)
            .map(|v| v.extrude[1].abs())
            .fold(0.0, f32::max);
        assert_eq!(core, 2.0 - AA / 2.0);
        // Every position lies on the centerline.
        assert!(m.vertices.iter().all(|v| v.pos[1] == 100.0));
    }

    #[test]
    fn joins_caps_and_dashes_build() {
        for join in [Join::Miter, Join::Bevel, Join::Round] {
            for cap in [Cap::Butt, Cap::Round, Cap::Square] {
                let mut m = MeshBuilder::new();
                let s = LineStyle {
                    join,
                    cap,
                    ..style(6.0)
                };
                m.line(
                    &[
                        [100.0, 100.0],
                        [500.0, 100.0],
                        [500.0, 500.0],
                        [100.0, 520.0],
                        [90.0, 100.0],
                    ],
                    SQ,
                    &s,
                );
                valid(&m);
                assert!(!m.is_empty());
            }
        }
        let mut solid = MeshBuilder::new();
        solid.line(&[[0.0, 50.0], [4000.0, 50.0]], SQ, &style(2.0));
        let mut dashed = MeshBuilder::new();
        let s = LineStyle {
            dashes: &[2.0, 2.0],
            ..style(2.0)
        };
        dashed.line(&[[0.0, 50.0], [4000.0, 50.0]], SQ, &s);
        valid(&dashed);
        // Dashes of 4 points at 8 units a point: 32 units on, 32 off, so
        // 4000 units hold 63 dashes.
        let dash_vertices = 16;
        assert_eq!(dashed.vertices.len(), 63 * dash_vertices);
        assert!(solid.vertices.len() < dashed.vertices.len());
    }

    #[test]
    fn lines_clip_and_hairlines_fade() {
        let mut m = MeshBuilder::new();
        m.line(&[[-500.0, 10.0], [-100.0, 10.0]], SQ, &style(2.0));
        assert!(m.is_empty(), "outside the square");
        m.line(&[[100.0, 10.0], [200.0, 10.0]], SQ, &style(0.5));
        valid(&m);
        assert!(m.vertices.iter().all(|v| v.color[3] == 0.5));
    }

    #[test]
    fn circles_are_discs_with_rings() {
        let mut m = MeshBuilder::new();
        m.circle([10.0, 10.0], 6.0, INK, Some((2.0, [1.0, 1.0, 1.0, 1.0])));
        valid(&m);
        let max = m
            .vertices
            .iter()
            .map(|v| v.extrude[0].hypot(v.extrude[1]))
            .fold(0.0, f32::max);
        assert!((max - (8.0 + AA / 2.0)).abs() < 1e-4, "{max}");
    }
}
