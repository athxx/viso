//! Compact vector-path storage (§13).
//!
//! [`PathArena`] is the storage form of a vector outline: a **structure-of-arrays**
//! command stream — one `u8` tag per command in `tags`, and its coordinates
//! appended as bare `f32` pairs into `points`. There is no per-segment enum,
//! `Box`, or vtable (§13.2); a cubic is one `Cubic` tag plus six packed floats,
//! not a heap object.
//!
//! Curved conveniences (`arc`, `conic`) lower to canonical `Quad`/`Cubic`
//! segments at build time (§13.1), so the stored stream is always one of the
//! five canonical commands — a consumer (D3.2 flatten/tessellate) never sees an
//! `Arc` or `Conic` tag.
//!
//! Creation-time [`PathMetadata`] (bounds, segment count, a convexity hint, a
//! simple-shape hint, a complexity score) is computed incrementally as commands
//! are pushed, so the command stream is scanned **once at build time, never
//! per render** (§13). The [`FillRule`] (`NonZero`/`EvenOdd`, §13.3) is carried
//! on the arena.
//!
//! This module is CPU-only storage. Flattening, tessellation, and GPU geometry
//! are the vector-mesh lane (D3.2) and consume this arena via [`PathArena::cmds`].

use crate::primitive::{PathCmd, Point, Rect};

/// The interior-fill winding rule for a path (§13.3).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum FillRule {
    /// A point is inside when the signed crossing count of a ray is non-zero.
    /// The default; matches how overlapping subpaths of the same orientation
    /// union rather than punch holes.
    #[default]
    NonZero,
    /// A point is inside when the crossing count is odd. Overlapping subpaths of
    /// the same orientation punch holes (the classic even-odd behaviour).
    EvenOdd,
}

/// The canonical command tags stored in [`PathArena::tags`], one `u8` per
/// command. Values are the compact wire codes; each tag consumes a fixed number
/// of `f32` pairs from [`PathArena::points`].
///
/// `Arc`/`Conic` are intentionally absent — they lower to `Quad`/`Cubic` at
/// build time (§13.1), so the stored stream is always canonical.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Tag {
    /// Begin a subpath: consumes 1 point.
    Move = 0,
    /// Line to a point: consumes 1 point.
    Line = 1,
    /// Quadratic Bézier: consumes 2 points (control, end).
    Quad = 2,
    /// Cubic Bézier: consumes 3 points (control0, control1, end).
    Cubic = 3,
    /// Close the current subpath: consumes 0 points.
    Close = 4,
}

impl Tag {
    /// How many `(x, y)` point pairs this tag consumes from `points`.
    const fn point_count(self) -> usize {
        match self {
            Tag::Move | Tag::Line => 1,
            Tag::Quad => 2,
            Tag::Cubic => 3,
            Tag::Close => 0,
        }
    }

    /// Decode a stored tag byte. Panics on an unknown byte — the arena only ever
    /// writes the five canonical codes, so a foreign byte is an invariant break.
    const fn from_u8(b: u8) -> Tag {
        match b {
            0 => Tag::Move,
            1 => Tag::Line,
            2 => Tag::Quad,
            3 => Tag::Cubic,
            4 => Tag::Close,
            _ => panic!("PathArena: corrupt command tag"),
        }
    }
}

/// A coarse convexity classification computed at build time (§13). A hint, not a
/// proof: `Convex` means every turn kept the same sign and no subpath restart
/// occurred, so the fill lane may take a fan-triangulation fast path; anything
/// else is `Concave` and takes the general path.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ConvexityHint {
    /// Not yet enough segments to decide (0–2 vertices).
    #[default]
    Unknown,
    /// Single subpath whose turns never changed sign — safe to fan-fill.
    Convex,
    /// Multiple subpaths, or turns of mixed sign — needs the general fill.
    Concave,
}

/// A build-time recognition hint for outlines that are exactly a common shape,
/// letting the renderer route them to an analytic primitive instead of the mesh
/// lane (§13). Conservative: only set when the command pattern matches exactly.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SimpleShapeHint {
    /// No recognised simple shape (the general case).
    #[default]
    None,
    /// A single closed axis-aligned rectangle (`Move`,`Line`×3,`Close`, or the
    /// same with an implicit closing edge).
    Rect,
    /// A single closed 4-arc/curve loop with rectangular control bounds — a
    /// circle or axis-aligned ellipse.
    Ellipse,
}

/// Creation-time path metadata (§13). Computed incrementally while commands are
/// pushed so the command stream is never re-scanned per render.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PathMetadata {
    /// Tight bounds over all control and end points (physical pixels). For a
    /// curved segment this bounds the control hull, which encloses the curve.
    pub bounds: Rect,
    /// Number of drawing commands (every tag except the leading `Move` of each
    /// subpath and `Close` — i.e. line/quad/cubic count).
    pub segment_count: u32,
    /// Convexity classification (see [`ConvexityHint`]).
    pub convex: ConvexityHint,
    /// Simple-shape recognition (see [`SimpleShapeHint`]).
    pub simple_shape: SimpleShapeHint,
    /// A cheap cost proxy: line = 1, quad = 2, cubic = 3, weighting the
    /// tessellation work a consumer should expect. Never exact; used to bucket
    /// paths for scheduling.
    pub complexity: u32,
}

impl Default for PathMetadata {
    fn default() -> Self {
        PathMetadata {
            bounds: Rect::ZERO,
            segment_count: 0,
            convex: ConvexityHint::Unknown,
            simple_shape: SimpleShapeHint::None,
            complexity: 0,
        }
    }
}

/// Compact structure-of-arrays vector path (§13.2).
///
/// Build with [`move_to`](Self::move_to)/[`line_to`](Self::line_to)/
/// [`quad_to`](Self::quad_to)/[`cubic_to`](Self::cubic_to)/[`close`](Self::close);
/// [`arc`](Self::arc)/[`conic`](Self::conic) lower to canonical segments. The
/// [`metadata`](Self::metadata) is kept current on every push. Iterate the
/// canonical command stream with [`cmds`](Self::cmds).
#[derive(Debug, Clone, PartialEq)]
pub struct PathArena {
    /// One canonical command tag per command, in order.
    tags: Vec<u8>,
    /// Packed `x, y` coordinates; each tag consumes `Tag::point_count()` pairs.
    points: Vec<f32>,
    /// Interior fill rule (§13.3).
    fill_rule: FillRule,
    /// Creation-time metadata, kept current as commands are pushed.
    meta: PathMetadata,
    /// The current point (end of the last segment), for relative reasoning and
    /// the arc/conic lowerings. `None` before the first `move_to`.
    current: Option<Point>,
    /// The start of the current subpath (target of `close`).
    subpath_start: Option<Point>,
    /// Running convexity state: sign of the last turn (`0` = none yet), and
    /// whether more than one subpath has been started (forces `Concave`).
    turn_sign: i8,
    /// The previous edge direction, for turn-sign accumulation.
    prev_dir: Option<Point>,
    /// Whether a second subpath (or a mid-path `move_to`) has occurred.
    multi_subpath: bool,
}

impl Default for PathArena {
    fn default() -> Self {
        PathArena::new()
    }
}

impl PathArena {
    /// An empty arena with the default [`FillRule::NonZero`].
    pub fn new() -> PathArena {
        PathArena {
            tags: Vec::new(),
            points: Vec::new(),
            fill_rule: FillRule::NonZero,
            meta: PathMetadata::default(),
            current: None,
            subpath_start: None,
            turn_sign: 0,
            prev_dir: None,
            multi_subpath: false,
        }
    }

    /// An empty arena preallocated for `segments` commands (tags + a generous
    /// point budget). Reused build scratch avoids reallocation on large icons.
    pub fn with_capacity(segments: usize) -> PathArena {
        let mut a = PathArena::new();
        a.tags.reserve(segments);
        a.points.reserve(segments * 4);
        a
    }

    /// Set the interior fill rule (§13.3). Metadata is unaffected.
    pub fn set_fill_rule(&mut self, rule: FillRule) {
        self.fill_rule = rule;
    }

    /// The interior fill rule.
    pub fn fill_rule(&self) -> FillRule {
        self.fill_rule
    }

    /// The creation-time metadata (§13) — always current, never re-scanned.
    pub fn metadata(&self) -> &PathMetadata {
        &self.meta
    }

    /// Number of stored commands (tags).
    pub fn command_count(&self) -> usize {
        self.tags.len()
    }

    /// Whether the arena holds no commands.
    pub fn is_empty(&self) -> bool {
        self.tags.is_empty()
    }

    /// Begin a new subpath at `p`. A `move_to` after existing commands starts a
    /// second subpath, which forces the convexity hint to `Concave`.
    pub fn move_to(&mut self, p: Point) {
        if !self.tags.is_empty() {
            self.multi_subpath = true;
        }
        self.push(Tag::Move, &[p]);
        self.current = Some(p);
        self.subpath_start = Some(p);
        self.prev_dir = None;
        self.recompute_convexity();
    }

    /// Straight line from the current point to `p`.
    pub fn line_to(&mut self, p: Point) {
        let from = self.current_or(p);
        self.push(Tag::Line, &[p]);
        self.accumulate_turn(from, p);
        self.current = Some(p);
    }

    /// Quadratic Bézier with control `c` ending at `p`.
    pub fn quad_to(&mut self, c: Point, p: Point) {
        let from = self.current_or(c);
        self.push(Tag::Quad, &[c, p]);
        // Turn accounting uses the chord (from→p) plus the control tangent; a
        // curved segment past the first is treated as bending, so any curve
        // beyond a single one falls out of the convex fast path conservatively.
        self.accumulate_turn(from, c);
        self.accumulate_turn(c, p);
        self.current = Some(p);
    }

    /// Cubic Bézier with controls `c0`,`c1` ending at `p`.
    pub fn cubic_to(&mut self, c0: Point, c1: Point, p: Point) {
        let from = self.current_or(c0);
        self.push(Tag::Cubic, &[c0, c1, p]);
        self.accumulate_turn(from, c0);
        self.accumulate_turn(c0, c1);
        self.accumulate_turn(c1, p);
        self.current = Some(p);
    }

    /// Close the current subpath with an implicit line back to its start.
    pub fn close(&mut self) {
        if let (Some(from), Some(start)) = (self.current, self.subpath_start) {
            self.accumulate_turn(from, start);
        }
        self.push(Tag::Close, &[]);
        self.current = self.subpath_start;
        self.recognize_simple_shape();
    }

    /// Append a circular arc, lowering it to canonical cubic segments (§13.1).
    ///
    /// The arc is centered at `center` with `radius`, sweeping from `start_rad`
    /// through `sweep_rad` (positive = counter-clockwise in a y-down space). It
    /// is split into ≤90° pieces, each an exact-as-cubic circular approximation
    /// (the standard `k = 4/3·tan(θ/4)` control-length rule). A `move_to` to the
    /// arc start is emitted first when there is no current point.
    pub fn arc(&mut self, center: Point, radius: f32, start_rad: f32, sweep_rad: f32) {
        if sweep_rad == 0.0 || radius <= 0.0 {
            return;
        }
        let start_pt = arc_point(center, radius, start_rad);
        if self.current.is_none() {
            self.move_to(start_pt);
        } else {
            self.line_to(start_pt);
        }

        // Split into segments of at most 90° so the cubic approximation stays
        // within circle tolerance.
        let seg_count = (sweep_rad.abs() / std::f32::consts::FRAC_PI_2)
            .ceil()
            .max(1.0) as usize;
        let seg_sweep = sweep_rad / seg_count as f32;
        let k = (4.0 / 3.0) * (seg_sweep / 4.0).tan();

        let mut a0 = start_rad;
        for _ in 0..seg_count {
            let a1 = a0 + seg_sweep;
            let p0 = arc_point(center, radius, a0);
            let p1 = arc_point(center, radius, a1);
            // Tangent-scaled control points (tangent = derivative of the circle
            // parametrization, length k·radius).
            let t0 = arc_tangent(a0);
            let t1 = arc_tangent(a1);
            let c0 = Point::new(p0.x + k * radius * t0.x, p0.y + k * radius * t0.y);
            let c1 = Point::new(p1.x - k * radius * t1.x, p1.y - k * radius * t1.y);
            self.cubic_to(c0, c1, p1);
            a0 = a1;
        }
    }

    /// Append a rational quadratic (conic) of `weight`, lowering it to canonical
    /// quadratic segments (§13.1).
    ///
    /// `weight == 1` is an ordinary quadratic and is emitted directly. Otherwise
    /// the conic is subdivided (the standard de Casteljau conic split) until each
    /// piece is close enough to a plain quadratic, matching how SVG/`Arc`-family
    /// inputs reduce to the canonical stream.
    pub fn conic(&mut self, c: Point, p: Point, weight: f32) {
        let from = self.current_or(c);
        conic_to_quads(from, c, p, weight, 0, self);
    }

    /// Iterate the stored stream as canonical [`PathCmd`]s. This reconstructs the
    /// authoring enum on the fly from the SoA — for consumers (the D3.2 flatten/
    /// tessellate lane) that want a command view without the arena owning a
    /// `Vec<PathCmd>`.
    pub fn cmds(&self) -> impl Iterator<Item = PathCmd> + '_ {
        CmdIter {
            tags: &self.tags,
            points: &self.points,
            tag_i: 0,
            pt_i: 0,
        }
    }

    /// Materialize the canonical command stream into a `Vec` (convenience for
    /// call sites that need an owned slice; prefer [`cmds`](Self::cmds) to avoid
    /// the allocation).
    pub fn to_cmds(&self) -> Vec<PathCmd> {
        self.cmds().collect()
    }

    // -- internals ---------------------------------------------------------

    /// Push one command: its tag plus its points, updating bounds/counts.
    fn push(&mut self, tag: Tag, pts: &[Point]) {
        debug_assert_eq!(tag.point_count(), pts.len());
        self.tags.push(tag as u8);
        for &p in pts {
            self.points.push(p.x);
            self.points.push(p.y);
            self.grow_bounds(p);
        }
        match tag {
            Tag::Line => {
                self.meta.segment_count += 1;
                self.meta.complexity += 1;
            }
            Tag::Quad => {
                self.meta.segment_count += 1;
                self.meta.complexity += 2;
            }
            Tag::Cubic => {
                self.meta.segment_count += 1;
                self.meta.complexity += 3;
            }
            Tag::Move | Tag::Close => {}
        }
    }

    /// The current point, or `fallback` if no subpath has started (a degenerate
    /// input; the fallback keeps the stream well-formed rather than panicking).
    fn current_or(&self, fallback: Point) -> Point {
        self.current.unwrap_or(fallback)
    }

    /// Expand `meta.bounds` to include `p`. The first point seeds the bounds.
    fn grow_bounds(&mut self, p: Point) {
        if self.meta.segment_count == 0 && self.tags.len() == 1 {
            // Very first stored command's first point: seed to a zero-size rect.
            self.meta.bounds = Rect {
                x: p.x,
                y: p.y,
                w: 0.0,
                h: 0.0,
            };
            return;
        }
        let b = self.meta.bounds;
        let x0 = b.x.min(p.x);
        let y0 = b.y.min(p.y);
        let x1 = (b.x + b.w).max(p.x);
        let y1 = (b.y + b.h).max(p.y);
        self.meta.bounds = Rect {
            x: x0,
            y: y0,
            w: x1 - x0,
            h: y1 - y0,
        };
    }

    /// Fold one edge (`from`→`to`) into the running turn-sign state.
    fn accumulate_turn(&mut self, from: Point, to: Point) {
        let dir = Point::new(to.x - from.x, to.y - from.y);
        if dir.x == 0.0 && dir.y == 0.0 {
            return;
        }
        if let Some(prev) = self.prev_dir {
            let cross = prev.x * dir.y - prev.y * dir.x;
            let sign = if cross > 0.0 {
                1
            } else if cross < 0.0 {
                -1
            } else {
                0
            };
            if sign != 0 {
                if self.turn_sign == 0 {
                    self.turn_sign = sign;
                } else if self.turn_sign != sign {
                    // A turn reversed direction → concave.
                    self.turn_sign = 2; // sentinel: mixed
                }
            }
        }
        self.prev_dir = Some(dir);
        self.recompute_convexity();
    }

    /// Recompute the convexity hint from the running state.
    fn recompute_convexity(&mut self) {
        self.meta.convex = if self.multi_subpath || self.turn_sign == 2 {
            ConvexityHint::Concave
        } else if self.meta.segment_count >= 3 && self.turn_sign != 0 {
            ConvexityHint::Convex
        } else {
            ConvexityHint::Unknown
        };
    }

    /// On `close`, check whether the just-closed single subpath is exactly a
    /// simple shape and set the hint. Only fires for a single-subpath arena.
    fn recognize_simple_shape(&mut self) {
        if self.multi_subpath {
            self.meta.simple_shape = SimpleShapeHint::None;
            return;
        }
        // A rectangle: Move + 3 Line + Close, all corners axis-aligned.
        if self.tags.len() == 5
            && self.tags[0] == Tag::Move as u8
            && self.tags[1] == Tag::Line as u8
            && self.tags[2] == Tag::Line as u8
            && self.tags[3] == Tag::Line as u8
            && self.tags[4] == Tag::Close as u8
            && self.is_axis_aligned_rect()
        {
            self.meta.simple_shape = SimpleShapeHint::Rect;
            return;
        }
        // An ellipse/circle: Move + 4 curve + Close (the 4-arc construction).
        if self.tags.len() == 6
            && self.tags[0] == Tag::Move as u8
            && (1..=4).all(|i| self.tags[i] == Tag::Cubic as u8 || self.tags[i] == Tag::Quad as u8)
            && self.tags[5] == Tag::Close as u8
        {
            self.meta.simple_shape = SimpleShapeHint::Ellipse;
            return;
        }
        self.meta.simple_shape = SimpleShapeHint::None;
    }

    /// Whether the four `Move`/`Line` end points form an axis-aligned rectangle.
    fn is_axis_aligned_rect(&self) -> bool {
        // Points are the 4 stored corners (Move, Line, Line, Line); Close has 0.
        if self.points.len() < 8 {
            return false;
        }
        let p = |i: usize| Point::new(self.points[i * 2], self.points[i * 2 + 1]);
        let (a, b, c, d) = (p(0), p(1), p(2), p(3));
        // Consecutive edges must alternate horizontal/vertical.
        let horiz = |u: Point, v: Point| (u.y - v.y).abs() < f32::EPSILON;
        let vert = |u: Point, v: Point| (u.x - v.x).abs() < f32::EPSILON;
        (horiz(a, b) && vert(b, c) && horiz(c, d) && vert(d, a))
            || (vert(a, b) && horiz(b, c) && vert(c, d) && horiz(d, a))
    }
}

/// Point on a circle at `angle` (radians), y-down.
fn arc_point(center: Point, radius: f32, angle: f32) -> Point {
    Point::new(
        center.x + radius * angle.cos(),
        center.y + radius * angle.sin(),
    )
}

/// Unit tangent of the circle parametrization at `angle` (y-down, CCW positive).
fn arc_tangent(angle: f32) -> Point {
    Point::new(-angle.sin(), angle.cos())
}

/// Maximum conic subdivision depth (prevents runaway recursion on degenerate
/// weights; 8 levels = 256 quads, far past any UI-icon need).
const CONIC_MAX_DEPTH: u32 = 8;

/// Subdivide a rational quadratic into plain quadratics, appending `quad_to`s.
fn conic_to_quads(from: Point, c: Point, p: Point, weight: f32, depth: u32, arena: &mut PathArena) {
    // weight ~1 (or depth exhausted): treat as a plain quadratic.
    if depth >= CONIC_MAX_DEPTH || (weight - 1.0).abs() <= 0.05 {
        arena.quad_to(c, p);
        return;
    }
    // Standard conic de Casteljau split at the parameter midpoint.
    let w = weight;
    let s = ((1.0 + w) * 0.5).sqrt(); // sub-weight of the two halves
    let mid_ctrl0 = lerp_weighted(from, c, w);
    let mid_ctrl1 = lerp_weighted(c, p, w);
    let mid = midpoint_w(mid_ctrl0, mid_ctrl1);
    conic_to_quads(from, mid_ctrl0, mid, s, depth + 1, arena);
    conic_to_quads(mid, mid_ctrl1, p, s, depth + 1, arena);
}

/// The rational midpoint control between `a` and `b` at weight `w`.
fn lerp_weighted(a: Point, b: Point, w: f32) -> Point {
    // Control point of a half-conic: (a + w·b) / (1 + w).
    let inv = 1.0 / (1.0 + w);
    Point::new((a.x + w * b.x) * inv, (a.y + w * b.y) * inv)
}

fn midpoint_w(a: Point, b: Point) -> Point {
    Point::new((a.x + b.x) * 0.5, (a.y + b.y) * 0.5)
}

/// Reconstructs [`PathCmd`]s from the SoA tag/point streams.
struct CmdIter<'a> {
    tags: &'a [u8],
    points: &'a [f32],
    tag_i: usize,
    pt_i: usize,
}

impl Iterator for CmdIter<'_> {
    type Item = PathCmd;

    fn next(&mut self) -> Option<PathCmd> {
        let tag = Tag::from_u8(*self.tags.get(self.tag_i)?);
        self.tag_i += 1;
        let mut take = || {
            let p = Point::new(self.points[self.pt_i], self.points[self.pt_i + 1]);
            self.pt_i += 2;
            p
        };
        Some(match tag {
            Tag::Move => PathCmd::MoveTo(take()),
            Tag::Line => PathCmd::LineTo(take()),
            Tag::Quad => {
                let c = take();
                PathCmd::QuadTo(c, take())
            }
            Tag::Cubic => {
                let c0 = take();
                let c1 = take();
                PathCmd::CubicTo(c0, c1, take())
            }
            Tag::Close => PathCmd::Close,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pt(x: f32, y: f32) -> Point {
        Point::new(x, y)
    }

    #[test]
    fn tags_and_points_are_packed_soa() {
        let mut a = PathArena::new();
        a.move_to(pt(0.0, 0.0));
        a.line_to(pt(10.0, 0.0));
        a.cubic_to(pt(10.0, 5.0), pt(5.0, 10.0), pt(0.0, 10.0));
        a.close();
        // 4 tags, no per-segment objects.
        assert_eq!(a.command_count(), 4);
        // points: Move(1) + Line(1) + Cubic(3) = 5 pairs = 10 floats.
        assert_eq!(a.points.len(), 10);
        // Round-trips to canonical commands.
        let cmds = a.to_cmds();
        assert_eq!(cmds.len(), 4);
        assert_eq!(cmds[0], PathCmd::MoveTo(pt(0.0, 0.0)));
        assert_eq!(
            cmds[2],
            PathCmd::CubicTo(pt(10.0, 5.0), pt(5.0, 10.0), pt(0.0, 10.0))
        );
        assert_eq!(cmds[3], PathCmd::Close);
    }

    #[test]
    fn metadata_bounds_cover_all_control_points() {
        let mut a = PathArena::new();
        a.move_to(pt(2.0, 3.0));
        a.line_to(pt(12.0, 3.0));
        a.quad_to(pt(20.0, 8.0), pt(12.0, 13.0));
        let b = a.metadata().bounds;
        assert_eq!(b.x, 2.0);
        assert_eq!(b.y, 3.0);
        assert_eq!(b.x + b.w, 20.0);
        assert_eq!(b.y + b.h, 13.0);
        assert_eq!(a.metadata().segment_count, 2);
        // line=1 + quad=2.
        assert_eq!(a.metadata().complexity, 3);
    }

    #[test]
    fn convexity_hint_is_convex_for_a_ccw_triangle() {
        let mut a = PathArena::new();
        a.move_to(pt(0.0, 0.0));
        a.line_to(pt(10.0, 0.0));
        a.line_to(pt(10.0, 10.0));
        a.line_to(pt(0.0, 10.0));
        a.close();
        assert_eq!(a.metadata().convex, ConvexityHint::Convex);
    }

    #[test]
    fn convexity_hint_is_concave_for_mixed_turns() {
        // An arrow / dart shape reverses turn direction.
        let mut a = PathArena::new();
        a.move_to(pt(0.0, 0.0));
        a.line_to(pt(10.0, 5.0));
        a.line_to(pt(0.0, 10.0));
        a.line_to(pt(3.0, 5.0)); // notch back in — reverses sign
        a.close();
        assert_eq!(a.metadata().convex, ConvexityHint::Concave);
    }

    #[test]
    fn convexity_hint_is_concave_for_two_subpaths() {
        let mut a = PathArena::new();
        a.move_to(pt(0.0, 0.0));
        a.line_to(pt(10.0, 0.0));
        a.line_to(pt(10.0, 10.0));
        a.close();
        a.move_to(pt(20.0, 20.0));
        a.line_to(pt(30.0, 20.0));
        a.line_to(pt(30.0, 30.0));
        a.close();
        assert_eq!(a.metadata().convex, ConvexityHint::Concave);
    }

    #[test]
    fn simple_shape_recognizes_an_axis_aligned_rect() {
        let mut a = PathArena::new();
        a.move_to(pt(0.0, 0.0));
        a.line_to(pt(10.0, 0.0));
        a.line_to(pt(10.0, 8.0));
        a.line_to(pt(0.0, 8.0));
        a.close();
        assert_eq!(a.metadata().simple_shape, SimpleShapeHint::Rect);
    }

    #[test]
    fn simple_shape_none_for_a_skewed_quad() {
        let mut a = PathArena::new();
        a.move_to(pt(0.0, 0.0));
        a.line_to(pt(10.0, 1.0)); // not horizontal
        a.line_to(pt(10.0, 8.0));
        a.line_to(pt(0.0, 8.0));
        a.close();
        assert_eq!(a.metadata().simple_shape, SimpleShapeHint::None);
    }

    #[test]
    fn arc_lowers_to_cubic_segments_only() {
        let mut a = PathArena::new();
        a.arc(pt(0.0, 0.0), 10.0, 0.0, std::f32::consts::PI); // half circle
        // No Arc tag survives; only Move + cubics.
        let cmds = a.to_cmds();
        assert!(matches!(cmds[0], PathCmd::MoveTo(_)));
        assert!(cmds[1..].iter().all(|c| matches!(c, PathCmd::CubicTo(..))));
        // A 180° sweep splits into 2 ≤90° cubics.
        assert_eq!(cmds.len(), 3);
        // Endpoint of the arc lands at the far side of the circle (≈ (-10, 0)).
        if let PathCmd::CubicTo(_, _, end) = cmds[2] {
            assert!((end.x - -10.0).abs() < 0.1);
            assert!(end.y.abs() < 0.1);
        } else {
            panic!("expected cubic");
        }
    }

    #[test]
    fn conic_with_unit_weight_is_one_quadratic() {
        let mut a = PathArena::new();
        a.move_to(pt(0.0, 0.0));
        a.conic(pt(5.0, 10.0), pt(10.0, 0.0), 1.0);
        let cmds = a.to_cmds();
        assert_eq!(cmds.len(), 2);
        assert_eq!(cmds[1], PathCmd::QuadTo(pt(5.0, 10.0), pt(10.0, 0.0)));
    }

    #[test]
    fn conic_with_nonunit_weight_lowers_to_quads_only() {
        let mut a = PathArena::new();
        a.move_to(pt(0.0, 0.0));
        a.conic(pt(5.0, 10.0), pt(10.0, 0.0), 2.0);
        let cmds = a.to_cmds();
        assert!(cmds.len() > 2); // subdivided
        assert!(cmds[1..].iter().all(|c| matches!(c, PathCmd::QuadTo(..))));
    }

    #[test]
    fn fill_rule_defaults_to_nonzero_and_is_settable() {
        let mut a = PathArena::new();
        assert_eq!(a.fill_rule(), FillRule::NonZero);
        a.set_fill_rule(FillRule::EvenOdd);
        assert_eq!(a.fill_rule(), FillRule::EvenOdd);
    }
}
