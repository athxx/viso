//! Paint primitives and their GPU instance layouts.
//!
//! A [`Primitive`] is the renderer-facing *description* of something to draw —
//! high-level Viso geometry (a rounded rect, a glyph run, …), independent of any
//! backend. Each primitive lowers to one `#[derive(GpuInstance)]` instance
//! struct whose `#[repr(C)]` layout the backend uploads directly.
//!
//! The instance struct's field names and formats are a three-way contract:
//! - the **shader** (D layer) declares them as an `InstanceSchema`,
//! - `#[derive(GpuInstance)]` records the real byte offsets (B layer),
//! - the headless rasterizer reads fields by that name (C layer).
//!
//! `create_pipeline` validates the derived layout against the schema, so a
//! mismatch is caught at pipeline-registration time.

use viso_gpu::{GpuInstance, TextureId};

// The Quad/Image/Mesh field contracts (`quad_schema`/`image_schema`/
// `mesh_schema`) live with the hand-written MSL in `viso-shader` (layer D);
// re-export them here so the instance structs and their schemas stay visibly
// paired at the primitive definition.
pub use viso_shader::{glyphrun_schema, image_schema, mesh_schema, quad_schema};

/// An axis-aligned rectangle in physical pixels, top-left origin.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Rect {
    /// Top-left corner x.
    pub x: f32,
    /// Top-left corner y.
    pub y: f32,
    /// Width.
    pub w: f32,
    /// Height.
    pub h: f32,
}

impl Rect {
    /// The intersection of two rects (both top-left-origin, physical pixels).
    ///
    /// Used to combine nested [`Primitive::Layer`] clip rects: a child clip is
    /// constrained to its parent. If the rects do not overlap, the result is an
    /// empty rect (`w`/`h` clamped to 0) — a clip that draws nothing.
    pub fn intersect(self, other: Rect) -> Rect {
        let x0 = self.x.max(other.x);
        let y0 = self.y.max(other.y);
        let x1 = (self.x + self.w).min(other.x + other.w);
        let y1 = (self.y + self.h).min(other.y + other.h);
        Rect {
            x: x0,
            y: y0,
            w: (x1 - x0).max(0.0),
            h: (y1 - y0).max(0.0),
        }
    }

    /// Whether the point `(px, py)` (physical px, same space as the rect) lies
    /// inside this rect. Near edges are inclusive, far edges exclusive
    /// (`[x, x+w)` / `[y, y+h)`), so two rects tiling a shared boundary do not
    /// both claim a point on that seam.
    #[inline]
    pub fn contains(self, px: f32, py: f32) -> bool {
        px >= self.x && px < self.x + self.w && py >= self.y && py < self.y + self.h
    }
}

/// A straight-alpha (non-premultiplied) linear RGBA color. The backend
/// premultiplies as needed; keeping the public type straight matches how
/// authors think about colors.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Rgba {
    /// Red, linear `[0, 1]`.
    pub r: f32,
    /// Green, linear `[0, 1]`.
    pub g: f32,
    /// Blue, linear `[0, 1]`.
    pub b: f32,
    /// Alpha `[0, 1]`.
    pub a: f32,
}

impl Rgba {
    /// A fully-transparent color.
    pub const TRANSPARENT: Rgba = Rgba {
        r: 0.0,
        g: 0.0,
        b: 0.0,
        a: 0.0,
    };
}

/// A border stroke drawn inside a quad's edge.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Border {
    /// Stroke width in pixels (0 = no border).
    pub width: f32,
    /// Stroke color.
    pub color: Rgba,
}

impl Border {
    /// No border.
    pub const NONE: Border = Border {
        width: 0.0,
        color: Rgba::TRANSPARENT,
    };
}

/// A rounded, optionally bordered, axis-aligned rectangle.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Quad {
    /// The rectangle, in physical pixels.
    pub rect: Rect,
    /// Fill color.
    pub color: Rgba,
    /// Corner radius in pixels (0 = sharp corners).
    pub radius: f32,
    /// Border stroke.
    pub border: Border,
}

impl Quad {
    /// Lower this quad to its GPU instance.
    pub fn to_instance(&self) -> QuadInstance {
        QuadInstance {
            rect_pos: [self.rect.x, self.rect.y],
            rect_size: [self.rect.w, self.rect.h],
            color: [self.color.r, self.color.g, self.color.b, self.color.a],
            radius: self.radius,
            border_width: self.border.width,
            border_color: [
                self.border.color.r,
                self.border.color.g,
                self.border.color.b,
                self.border.color.a,
            ],
        }
    }
}

/// A clip/compositing layer pushed by [`Primitive::Layer`].
///
/// Every following primitive is constrained to `clip` until the matching
/// [`Primitive::LayerEnd`]. `opacity` selects how the layer reaches the screen:
///
/// - `opacity == 1.0`: the layer is a plain rectangular clip container — the
///   subtree draws directly into the current target, bounded by an in-pass
///   hardware scissor. Nested layers intersect their clips.
/// - `opacity < 1.0`: the `Layer..LayerEnd` subtree is rendered into an
///   offscreen texture sized to `clip`, then composited back into the current
///   target as a single textured quad modulated by `opacity`. This makes a
///   whole layer uniformly translucent without double-blending its overlapping
///   contents, at the cost of one offscreen pass per translucent layer.
///
/// The offscreen pass is emitted before the main pass (see the render backend's
/// draw-list ordering) and cleared to transparent.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct LayerClip {
    /// The clip rectangle, in physical pixels. For a translucent layer this is
    /// also the offscreen texture's extent and world-space origin.
    pub clip: Rect,
    /// Layer opacity in `[0, 1]`. `1.0` clips in-pass; `< 1.0` triggers
    /// offscreen compositing at this opacity.
    pub opacity: f32,
}

/// A textured image: sample a sub-rect of `texture` into a destination `rect`,
/// modulated by `tint` (a = opacity).
///
/// `uv` is in **normalized** texture coordinates (`0..1` over the full texture),
/// so an atlas caller passes the sub-region occupied by its image; a whole-image
/// draw passes `Rect { x: 0, y: 0, w: 1, h: 1 }`. Carrying an explicit UV
/// sub-rect (rather than deriving it from a corner) is what lets the same
/// path serve glyph/atlas sub-regions in the text step.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ImageDraw {
    /// Destination rectangle, in physical pixels.
    pub rect: Rect,
    /// Source sub-rect in normalized texture coordinates (`0..1`).
    pub uv: Rect,
    /// Straight linear RGBA tint (a = opacity); `Rgba` white/1.0 = unmodified.
    pub tint: Rgba,
    /// The texture to sample.
    pub texture: TextureId,
}

impl ImageDraw {
    /// Lower this image to its GPU instance.
    pub fn to_instance(&self) -> ImageInstance {
        ImageInstance {
            rect_pos: [self.rect.x, self.rect.y],
            rect_size: [self.rect.w, self.rect.h],
            uv_pos: [self.uv.x, self.uv.y],
            uv_size: [self.uv.w, self.uv.h],
            color: [self.tint.r, self.tint.g, self.tint.b, self.tint.a],
        }
    }
}

/// One glyph of a [`GlyphRunDraw`]: where it lands on screen, which atlas
/// sub-rect holds its SDF, and how wide the SDF's coverage ramp is.
///
/// The text subsystem ([`viso_text`]) computes these — screen rect and atlas UV
/// are already resolved — so the renderer never re-runs layout. `px_range` is
/// the glyph's screen-pixels-per-SDF-unit, consumed by the shader to decode
/// coverage from the single-channel R8 signed-distance atlas.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct GlyphInstanceData {
    /// Destination rectangle on screen, in physical pixels.
    pub rect: Rect,
    /// Source sub-rect in the atlas, in normalized texture coordinates (`0..1`).
    pub uv: Rect,
    /// SDF coverage-ramp width (screen pixels per SDF unit) for shader decode.
    pub px_range: f32,
}

/// A run of shaped glyphs sharing one atlas texture and one color.
///
/// The glyphs are pre-laid-out by [`viso_text`]; the run carries them as flat
/// per-glyph instance data plus the atlas [`TextureId`] they sample. The whole
/// run is a single color (`color`) — Phase 2 does not support per-glyph color.
#[derive(Debug, Clone, PartialEq)]
pub struct GlyphRunDraw {
    /// The positioned glyphs, one screen quad each.
    pub glyphs: Vec<GlyphInstanceData>,
    /// The single-channel R8 SDF atlas the glyphs sample.
    pub atlas: TextureId,
    /// Straight linear RGBA color applied to the entire run (a = opacity).
    pub color: Rgba,
}

impl GlyphRunDraw {
    /// Lower one glyph to its GPU instance, applying the run's color.
    pub fn instance(&self, glyph: &GlyphInstanceData) -> GlyphInstance {
        GlyphInstance {
            rect_pos: [glyph.rect.x, glyph.rect.y],
            rect_size: [glyph.rect.w, glyph.rect.h],
            uv_pos: [glyph.uv.x, glyph.uv.y],
            uv_size: [glyph.uv.w, glyph.uv.h],
            color: [self.color.r, self.color.g, self.color.b, self.color.a],
            px_range: glyph.px_range,
        }
    }
}

/// A 2D point in physical pixels, top-left origin. The building block of
/// [`Path`] commands and mesh vertices.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Point {
    /// x in physical pixels.
    pub x: f32,
    /// y in physical pixels.
    pub y: f32,
}

impl Point {
    /// Construct a point.
    pub const fn new(x: f32, y: f32) -> Point {
        Point { x, y }
    }
}

/// One command of a [`Path`] outline. Curves are flattened to line segments by
/// the tessellator (De Casteljau, `tolerance`-bounded). Coordinates are physical
/// pixels, top-left origin.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum PathCmd {
    /// Begin a new subpath at this point.
    MoveTo(Point),
    /// Straight line from the current point.
    LineTo(Point),
    /// Quadratic Bézier: one control point, then the endpoint.
    QuadTo(Point, Point),
    /// Cubic Bézier: two control points, then the endpoint.
    CubicTo(Point, Point, Point),
    /// Close the current subpath (line back to its start).
    Close,
}

/// How consecutive stroke segments are joined at a corner.
///
/// A miter join extends the outer edges to a sharp point, falling back to a
/// bevel when the miter length exceeds `miter_limit × half_width`; a bevel join
/// always cuts the corner flat.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LineJoin {
    /// Sharp corner, bevel fallback past the miter limit.
    Miter,
    /// Flat-cut corner.
    Bevel,
}

/// A stroke (outline) applied to a [`Path`].
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Stroke {
    /// Stroke width in physical pixels (centered on the path).
    pub width: f32,
    /// Straight linear RGBA stroke color.
    pub color: Rgba,
    /// How corners are joined.
    pub join: LineJoin,
}

/// A filled and/or stroked vector path.
///
/// Phase 2's path support is deliberately minimal: the CPU tessellator flattens
/// curves, fan-triangulates the fill (assuming a simple, roughly convex
/// outline), and expands the stroke into segment quads with miter/bevel joins,
/// adding a 1px coverage-AA fringe. It does not implement even-odd/nonzero
/// winding fills of self-intersecting outlines, caps beyond a butt end, or
/// dashing — those are out of scope for this slice.
#[derive(Debug, Clone, PartialEq)]
pub struct Path {
    /// The outline commands.
    pub cmds: Vec<PathCmd>,
    /// Fill color, if the interior is painted.
    pub fill: Option<Rgba>,
    /// Stroke, if the outline is painted (drawn over the fill).
    pub stroke: Option<Stroke>,
}

/// A colored triangle mesh supplied directly by the caller (no tessellation).
///
/// `vertices`/`indices` are consumed as-is: each vertex carries a position, a
/// straight linear color, and an AA `edge` weight (`1` interior, `0` fringe),
/// exactly matching [`MeshVertex`]. This is the escape hatch for geometry Viso's
/// higher-level primitives don't cover; [`Path`] lowers into the same buffers.
#[derive(Debug, Clone, PartialEq)]
pub struct Mesh {
    /// The mesh vertices.
    pub vertices: Vec<MeshVertex>,
    /// Triangle-list indices into `vertices` (3 per triangle).
    pub indices: Vec<u32>,
}

/// Renderer-facing primitive. This expresses *only* what the renderer needs —
/// it is neither a component nor a node. One node may emit several
/// primitives; primitives of the same kind may batch into one draw call.
///
/// The primitive stream is **flat**: [`Primitive::Layer`] pushes a clip and
/// [`Primitive::LayerEnd`] pops it (a push/pop clip stack). Nested layers intersect their
/// clip rects. This keeps `Vec<Primitive>` batchable rather than a recursive
/// tree.
///
/// Every variant carries its draw data.
#[derive(Debug, Clone, PartialEq)]
pub enum Primitive {
    /// A rounded/bordered rectangle.
    Quad(Quad),
    /// A run of shaped glyphs sampling a single-channel SDF atlas.
    GlyphRun(GlyphRunDraw),
    /// A textured image sampled into a rect.
    Image(ImageDraw),
    /// A filled/stroked vector path.
    Path(Path),
    /// A colored triangle mesh.
    Mesh(Mesh),
    /// Push a clip/compositing layer. Following primitives are constrained to
    /// its clip (intersected with any enclosing layer's clip) until the matching
    /// [`Primitive::LayerEnd`]. A `LayerClip::opacity < 1` layer additionally
    /// renders its subtree offscreen and composites it back at that opacity.
    Layer(LayerClip),
    /// Pop the most recent [`Primitive::Layer`] clip (and, for a translucent
    /// layer, close its offscreen pass and emit the composite).
    LayerEnd,
}

/// GPU instance for the Quad built-in shader.
///
/// Field names/formats match [`quad_schema`] and the headless `fill_quad`
/// reader. Colors are **straight** (non-premultiplied) linear RGBA — the
/// backend premultiplies. `#[repr(C)]` with only 4-byte-aligned scalars, so the
/// derive's `offset_of!`-based layout has no padding surprises.
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, GpuInstance)]
pub struct QuadInstance {
    /// Top-left corner in physical pixels.
    pub rect_pos: [f32; 2],
    /// Width/height in physical pixels.
    pub rect_size: [f32; 2],
    /// Straight linear RGBA fill.
    pub color: [f32; 4],
    /// Corner radius in pixels.
    pub radius: f32,
    /// Border stroke width in pixels (0 = none).
    pub border_width: f32,
    /// Straight linear RGBA border color.
    pub border_color: [f32; 4],
}

/// GPU instance for the Image built-in shader.
///
/// Field names/formats match [`image_schema`] and the headless `fill_image`
/// reader. `color` is a **straight** (non-premultiplied) linear RGBA tint;
/// the sampled texel is premultiplied (Viso texture convention) and the shader
/// combines them. `#[repr(C)]` with only 8-byte `[f32; 2]`/`[f32; 4]` fields, so
/// the derive's `offset_of!`-based layout has no padding surprises.
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, GpuInstance)]
pub struct ImageInstance {
    /// Destination top-left in physical pixels.
    pub rect_pos: [f32; 2],
    /// Destination width/height in physical pixels.
    pub rect_size: [f32; 2],
    /// Source sub-rect origin in normalized texture coords.
    pub uv_pos: [f32; 2],
    /// Source sub-rect size in normalized texture coords.
    pub uv_size: [f32; 2],
    /// Straight linear RGBA tint (a = opacity).
    pub color: [f32; 4],
}

/// GPU instance for the GlyphRun built-in shader.
///
/// Field names/formats match [`glyphrun_schema`] and the headless `fill_glyph`
/// reader. Structurally the same as [`ImageInstance`] plus a `px_range` decode
/// factor: the sampled texel is a single-channel signed distance (in the R8
/// atlas's R channel), and the shader turns it into coverage via `px_range`.
/// `color` is a **straight** (non-premultiplied) linear RGBA; the shader
/// premultiplies. `#[repr(C)]` with 8-byte `[f32; 2]`/`[f32; 4]` fields and a
/// trailing 4-byte `f32`, so the derive's `offset_of!` layout has no padding.
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, GpuInstance)]
pub struct GlyphInstance {
    /// Destination top-left in physical pixels.
    pub rect_pos: [f32; 2],
    /// Destination width/height in physical pixels.
    pub rect_size: [f32; 2],
    /// Atlas sub-rect origin in normalized texture coords.
    pub uv_pos: [f32; 2],
    /// Atlas sub-rect size in normalized texture coords.
    pub uv_size: [f32; 2],
    /// Straight linear RGBA color for the whole run (a = opacity).
    pub color: [f32; 4],
    /// SDF coverage-ramp width (screen pixels per SDF unit).
    pub px_range: f32,
}

/// One vertex of the general triangle mesh built-in (shared by [`Path`] and
/// [`Mesh`]).
///
/// Field names/formats match [`mesh_schema`] and the headless mesh reader.
/// `color` is a **straight** (non-premultiplied) linear RGBA; the shader
/// premultiplies. `edge` is a `[0, 1]` coverage weight — `1` at interior
/// vertices, ramping to `0` at antialiased fringe vertices — interpolated across
/// the triangle so the fragment shader gets smooth edge coverage. Unlike the
/// quad/image instance structs this is *per-vertex* data drawn as an indexed
/// triangle list, not a `vertex_id`-generated quad. `#[repr(C)]`, stride 28.
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, GpuInstance)]
pub struct MeshVertex {
    /// Position in physical pixels, top-left origin.
    pub pos: [f32; 2],
    /// Straight linear RGBA.
    pub color: [f32; 4],
    /// Coverage-AA weight (`1` interior, `0` fringe).
    pub edge: f32,
}

/// The tolerance (max chord deviation, physical pixels) used when flattening
/// Bézier curves to line segments.
const FLATTEN_TOLERANCE: f32 = 0.25;

/// Width of the antialiasing fringe, in physical pixels, added around fills and
/// strokes. The fringe vertices carry `edge = 0`; interior vertices `edge = 1`.
const AA_FRINGE: f32 = 1.0;

/// The bevel-fallback threshold for miter joins, as a multiple of the stroke's
/// half-width. Corners sharper than this switch from miter to bevel.
const MITER_LIMIT: f32 = 4.0;

impl Path {
    /// Tessellate this path into `verts`/`indices` (appended; absolute indices).
    ///
    /// Fill is emitted first (so the stroke draws over it). Emitted vertices use
    /// the `MeshVertex` contract: straight color, `edge` coverage. This is the
    /// CPU half of the Path→mesh lowering; the GPU/headless mesh path renders the
    /// result as one indexed triangle list.
    pub fn tessellate(&self, verts: &mut Vec<MeshVertex>, indices: &mut Vec<u32>) {
        let subpaths = flatten(&self.cmds);

        if let Some(fill) = self.fill {
            for sub in &subpaths {
                fill_subpath(&sub.points, fill, verts, indices);
            }
        }
        if let Some(stroke) = self.stroke {
            for sub in &subpaths {
                stroke_subpath(&sub.points, sub.closed, stroke, verts, indices);
            }
        }
    }
}

/// A flattened subpath: a polyline plus whether it is closed.
struct Subpath {
    points: Vec<Point>,
    closed: bool,
}

/// Flatten path commands into subpaths of line segments (De Casteljau, bounded
/// by [`FLATTEN_TOLERANCE`]). Consecutive duplicate points are dropped so the
/// stroker never sees zero-length segments.
fn flatten(cmds: &[PathCmd]) -> Vec<Subpath> {
    let mut out: Vec<Subpath> = Vec::new();
    let mut cur: Vec<Point> = Vec::new();
    let mut closed = false;
    let mut start = Point::new(0.0, 0.0);

    let push = |cur: &mut Vec<Point>, p: Point| {
        if cur.last().map(|&l| l != p).unwrap_or(true) {
            cur.push(p);
        }
    };

    for &cmd in cmds {
        match cmd {
            PathCmd::MoveTo(p) => {
                if cur.len() >= 2 {
                    out.push(Subpath {
                        points: std::mem::take(&mut cur),
                        closed,
                    });
                } else {
                    cur.clear();
                }
                closed = false;
                start = p;
                cur.push(p);
            }
            PathCmd::LineTo(p) => push(&mut cur, p),
            PathCmd::QuadTo(c, p) => {
                let from = cur.last().copied().unwrap_or(c);
                flatten_quad(from, c, p, &mut cur);
            }
            PathCmd::CubicTo(c0, c1, p) => {
                let from = cur.last().copied().unwrap_or(c0);
                flatten_cubic(from, c0, c1, p, &mut cur);
            }
            PathCmd::Close => {
                closed = true;
                push(&mut cur, start);
            }
        }
    }
    if cur.len() >= 2 {
        out.push(Subpath {
            points: cur,
            closed,
        });
    }
    out
}

/// Recursively subdivide a quadratic Bézier until it is flat within tolerance,
/// appending the flattened points (excluding the start) to `out`.
fn flatten_quad(p0: Point, p1: Point, p2: Point, out: &mut Vec<Point>) {
    // Distance from the control point to the chord; a good flatness proxy.
    let d = point_line_dist(p1, p0, p2);
    if d <= FLATTEN_TOLERANCE {
        out.push(p2);
        return;
    }
    let p01 = midpoint(p0, p1);
    let p12 = midpoint(p1, p2);
    let mid = midpoint(p01, p12);
    flatten_quad(p0, p01, mid, out);
    flatten_quad(mid, p12, p2, out);
}

/// Recursively subdivide a cubic Bézier until it is flat within tolerance,
/// appending the flattened points (excluding the start) to `out`.
fn flatten_cubic(p0: Point, p1: Point, p2: Point, p3: Point, out: &mut Vec<Point>) {
    let d = point_line_dist(p1, p0, p3).max(point_line_dist(p2, p0, p3));
    if d <= FLATTEN_TOLERANCE {
        out.push(p3);
        return;
    }
    let p01 = midpoint(p0, p1);
    let p12 = midpoint(p1, p2);
    let p23 = midpoint(p2, p3);
    let p012 = midpoint(p01, p12);
    let p123 = midpoint(p12, p23);
    let mid = midpoint(p012, p123);
    flatten_cubic(p0, p01, p012, mid, out);
    flatten_cubic(mid, p123, p23, p3, out);
}

fn midpoint(a: Point, b: Point) -> Point {
    Point::new((a.x + b.x) * 0.5, (a.y + b.y) * 0.5)
}

/// Perpendicular distance from point `p` to the line through `a`,`b`.
fn point_line_dist(p: Point, a: Point, b: Point) -> f32 {
    let dx = b.x - a.x;
    let dy = b.y - a.y;
    let len = (dx * dx + dy * dy).sqrt();
    if len < 1e-6 {
        let ex = p.x - a.x;
        let ey = p.y - a.y;
        return (ex * ex + ey * ey).sqrt();
    }
    ((p.x - a.x) * dy - (p.y - a.y) * dx).abs() / len
}

/// Fan-triangulate a filled subpath from its centroid, with a 1px coverage-AA
/// fringe around the outline. Assumes a simple, roughly convex polygon (Phase 2
/// scope). No-op for degenerate outlines (< 3 points).
fn fill_subpath(
    points: &[Point],
    color: Rgba,
    verts: &mut Vec<MeshVertex>,
    indices: &mut Vec<u32>,
) {
    // Drop a duplicated closing point so the outline is a clean ring.
    let ring: &[Point] = match points.split_last() {
        Some((last, head)) if head.first() == Some(last) && head.len() >= 3 => head,
        _ => points,
    };
    if ring.len() < 3 {
        return;
    }

    let col = [color.r, color.g, color.b, color.a];
    let cx = ring.iter().map(|p| p.x).sum::<f32>() / ring.len() as f32;
    let cy = ring.iter().map(|p| p.y).sum::<f32>() / ring.len() as f32;
    let center = Point::new(cx, cy);

    // Interior ring: centroid + each outline point (edge = 1, full coverage).
    let center_idx = verts.len() as u32;
    verts.push(mesh_vert(center, col, 1.0));
    let inner_start = verts.len() as u32;
    for &p in ring {
        verts.push(mesh_vert(p, col, 1.0));
    }
    let n = ring.len() as u32;
    for i in 0..n {
        let a = inner_start + i;
        let b = inner_start + (i + 1) % n;
        indices.extend_from_slice(&[center_idx, a, b]);
    }

    // AA fringe: a ring of edge=0 vertices pushed outward along the outward
    // normal, bridged to the interior ring with two triangles per segment.
    let outer_start = verts.len() as u32;
    for i in 0..ring.len() {
        let p = ring[i];
        let normal = outward_normal(ring, i, center);
        let outer = Point::new(p.x + normal.0 * AA_FRINGE, p.y + normal.1 * AA_FRINGE);
        verts.push(mesh_vert(outer, col, 0.0));
    }
    for i in 0..n {
        let j = (i + 1) % n;
        let i0 = inner_start + i;
        let i1 = inner_start + j;
        let o0 = outer_start + i;
        let o1 = outer_start + j;
        indices.extend_from_slice(&[i0, o0, o1, i0, o1, i1]);
    }
}

/// The (approximate) outward unit normal at outline vertex `i`, using the two
/// adjacent edges and disambiguated against the polygon centroid.
fn outward_normal(ring: &[Point], i: usize, center: Point) -> (f32, f32) {
    let n = ring.len();
    let prev = ring[(i + n - 1) % n];
    let p = ring[i];
    let next = ring[(i + 1) % n];
    // Average the two edge directions, take the perpendicular.
    let d0 = norm(p.x - prev.x, p.y - prev.y);
    let d1 = norm(next.x - p.x, next.y - p.y);
    let tx = d0.0 + d1.0;
    let ty = d0.1 + d1.1;
    let (mut nx, mut ny) = norm(-ty, tx);
    // Flip so it points away from the centroid.
    if (p.x - center.x) * nx + (p.y - center.y) * ny < 0.0 {
        nx = -nx;
        ny = -ny;
    }
    (nx, ny)
}

/// Expand a polyline into a stroke: one quad per segment, plus miter/bevel joins
/// at interior vertices, with a coverage-AA fringe along both sides. Butt ends
/// (no caps) for open subpaths.
fn stroke_subpath(
    points: &[Point],
    closed: bool,
    stroke: Stroke,
    verts: &mut Vec<MeshVertex>,
    indices: &mut Vec<u32>,
) {
    if points.len() < 2 || stroke.width <= 0.0 {
        return;
    }
    let hw = stroke.width * 0.5;
    let col = [
        stroke.color.r,
        stroke.color.g,
        stroke.color.b,
        stroke.color.a,
    ];

    // Build the segment list (drop a duplicated closing point; closed rings wrap).
    let ring: &[Point] = match points.split_last() {
        Some((last, head)) if closed && head.first() == Some(last) => head,
        _ => points,
    };
    let count = ring.len();
    let seg_count = if closed { count } else { count - 1 };

    for s in 0..seg_count {
        let a = ring[s];
        let b = ring[(s + 1) % count];
        let dir = norm(b.x - a.x, b.y - a.y);
        // Left normal (perpendicular).
        let nx = -dir.1;
        let ny = dir.0;
        emit_stroke_quad(a, b, nx, ny, hw, col, verts, indices);

        // Join at `b` with the next segment (interior vertices only).
        let is_interior = closed || (s + 1) < seg_count;
        if is_interior {
            let c = ring[(s + 2) % count];
            emit_join(b, a, c, nx, ny, hw, stroke.join, col, verts, indices);
        }
    }
}

/// Emit one filled+fringed stroke quad for segment `a`→`b` with left normal
/// `(nx, ny)` and half-width `hw`.
#[allow(clippy::too_many_arguments)]
fn emit_stroke_quad(
    a: Point,
    b: Point,
    nx: f32,
    ny: f32,
    hw: f32,
    col: [f32; 4],
    verts: &mut Vec<MeshVertex>,
    indices: &mut Vec<u32>,
) {
    let base = verts.len() as u32;
    // Core quad corners (edge = 1) then fringe corners (edge = 0) on each side.
    let al = Point::new(a.x + nx * hw, a.y + ny * hw);
    let ar = Point::new(a.x - nx * hw, a.y - ny * hw);
    let bl = Point::new(b.x + nx * hw, b.y + ny * hw);
    let br = Point::new(b.x - nx * hw, b.y - ny * hw);
    verts.push(mesh_vert(al, col, 1.0)); // 0
    verts.push(mesh_vert(ar, col, 1.0)); // 1
    verts.push(mesh_vert(bl, col, 1.0)); // 2
    verts.push(mesh_vert(br, col, 1.0)); // 3
    indices.extend_from_slice(&[base, base + 1, base + 2, base + 1, base + 3, base + 2]);

    // Fringe on the left (+normal) and right (-normal) edges.
    let alf = Point::new(al.x + nx * AA_FRINGE, al.y + ny * AA_FRINGE);
    let blf = Point::new(bl.x + nx * AA_FRINGE, bl.y + ny * AA_FRINGE);
    let arf = Point::new(ar.x - nx * AA_FRINGE, ar.y - ny * AA_FRINGE);
    let brf = Point::new(br.x - nx * AA_FRINGE, br.y - ny * AA_FRINGE);
    let f = verts.len() as u32;
    verts.push(mesh_vert(alf, col, 0.0)); // f+0
    verts.push(mesh_vert(blf, col, 0.0)); // f+1
    verts.push(mesh_vert(arf, col, 0.0)); // f+2
    verts.push(mesh_vert(brf, col, 0.0)); // f+3
    // Left fringe bridges core edge (al=base, bl=base+2) to (alf, blf).
    indices.extend_from_slice(&[base, f, f + 1, base, f + 1, base + 2]);
    // Right fringe bridges core edge (ar=base+1, br=base+3) to (arf, brf).
    indices.extend_from_slice(&[base + 1, f + 2, f + 3, base + 1, f + 3, base + 3]);
}

/// Fill the wedge at corner `b` between the incoming segment (left normal
/// `(pnx, pny)`) and the outgoing segment toward `c`. Miter when within the
/// limit, otherwise a bevel triangle.
#[allow(clippy::too_many_arguments)]
fn emit_join(
    b: Point,
    a: Point,
    c: Point,
    pnx: f32,
    pny: f32,
    hw: f32,
    join: LineJoin,
    col: [f32; 4],
    verts: &mut Vec<MeshVertex>,
    indices: &mut Vec<u32>,
) {
    let ndir = norm(c.x - b.x, c.y - b.y);
    let nnx = -ndir.1;
    let nny = ndir.0;

    // Turn direction: cross of incoming dir and outgoing dir.
    let idir = norm(b.x - a.x, b.y - a.y);
    let cross = idir.0 * ndir.1 - idir.1 * ndir.0;
    if cross.abs() < 1e-4 {
        return; // straight — nothing to fill.
    }
    // Outer side is opposite the turn. For a left turn (cross > 0) the outer
    // corner is on the -normal side; for a right turn, the +normal side.
    let sign = if cross > 0.0 { -1.0 } else { 1.0 };
    let p_out = Point::new(b.x + sign * pnx * hw, b.y + sign * pny * hw);
    let n_out = Point::new(b.x + sign * nnx * hw, b.y + sign * nny * hw);

    let base = verts.len() as u32;
    verts.push(mesh_vert(b, col, 1.0));
    verts.push(mesh_vert(p_out, col, 1.0));
    verts.push(mesh_vert(n_out, col, 1.0));

    // Miter apex: intersection of the two outer edges. Fall back to bevel if the
    // miter grows past MITER_LIMIT × hw or the join kind is Bevel.
    if join == LineJoin::Miter
        && normals_diverge(pnx, pny, nnx, nny, sign)
        && let Some(apex) = miter_apex(p_out, idir, n_out, ndir)
    {
        let dx = apex.x - b.x;
        let dy = apex.y - b.y;
        if (dx * dx + dy * dy).sqrt() <= MITER_LIMIT * hw {
            let apex_idx = verts.len() as u32;
            verts.push(mesh_vert(apex, col, 1.0));
            indices.extend_from_slice(&[base, base + 1, apex_idx, base, apex_idx, base + 2]);
            return;
        }
    }
    // Bevel: single triangle across the corner.
    indices.extend_from_slice(&[base, base + 1, base + 2]);
}

/// Whether the two outer edge normals diverge enough that a miter apex is
/// meaningful (guards against near-parallel edges where the apex shoots to
/// infinity).
fn normals_diverge(pnx: f32, pny: f32, nnx: f32, nny: f32, sign: f32) -> bool {
    let dot = (sign * pnx) * (sign * nnx) + (sign * pny) * (sign * nny);
    dot < 0.999
}

/// Intersection of the outer edge line through `p` along `pdir` with the line
/// through `q` along `qdir`. `None` if near-parallel.
fn miter_apex(p: Point, pdir: (f32, f32), q: Point, qdir: (f32, f32)) -> Option<Point> {
    let denom = pdir.0 * qdir.1 - pdir.1 * qdir.0;
    if denom.abs() < 1e-6 {
        return None;
    }
    let t = ((q.x - p.x) * qdir.1 - (q.y - p.y) * qdir.0) / denom;
    Some(Point::new(p.x + pdir.0 * t, p.y + pdir.1 * t))
}

/// Normalize a 2D vector; returns `(0, 0)` for a near-zero input.
fn norm(x: f32, y: f32) -> (f32, f32) {
    let len = (x * x + y * y).sqrt();
    if len < 1e-6 {
        (0.0, 0.0)
    } else {
        (x / len, y / len)
    }
}

fn mesh_vert(p: Point, color: [f32; 4], edge: f32) -> MeshVertex {
    MeshVertex {
        pos: [p.x, p.y],
        color,
        edge,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn contains_is_inclusive_near_exclusive_far() {
        let r = Rect {
            x: 10.0,
            y: 20.0,
            w: 30.0,
            h: 40.0,
        };
        // Interior point hits.
        assert!(r.contains(15.0, 25.0));
        // Near corner is inclusive.
        assert!(r.contains(10.0, 20.0));
        // Far edges are exclusive (a point on x+w / y+h misses).
        assert!(!r.contains(40.0, 25.0));
        assert!(!r.contains(15.0, 60.0));
        assert!(!r.contains(40.0, 60.0));
        // Outside on any side misses.
        assert!(!r.contains(9.0, 25.0));
        assert!(!r.contains(15.0, 19.0));
    }

    #[test]
    fn zero_size_rect_contains_nothing() {
        let r = Rect {
            x: 5.0,
            y: 5.0,
            w: 0.0,
            h: 0.0,
        };
        // Its own origin is on the (coincident) far edge, so it never hits.
        assert!(!r.contains(5.0, 5.0));
    }

    #[test]
    fn quad_instance_layout_matches_schema() {
        // The derived layout must validate against the shader schema — the same
        // check `create_pipeline` performs at registration.
        assert_eq!(
            QuadInstance::LAYOUT.validate_against(&quad_schema()),
            Ok(())
        );
    }

    #[test]
    fn image_instance_layout_matches_schema() {
        assert_eq!(
            ImageInstance::LAYOUT.validate_against(&image_schema()),
            Ok(())
        );
    }

    #[test]
    fn glyphrun_instance_layout_matches_schema() {
        assert_eq!(
            GlyphInstance::LAYOUT.validate_against(&glyphrun_schema()),
            Ok(())
        );
    }

    #[test]
    fn glyphrun_lowers_to_instance_with_run_color() {
        let run = GlyphRunDraw {
            glyphs: vec![],
            atlas: TextureId(0),
            color: Rgba {
                r: 0.1,
                g: 0.2,
                b: 0.3,
                a: 0.9,
            },
        };
        let glyph = GlyphInstanceData {
            rect: Rect {
                x: 5.0,
                y: 6.0,
                w: 7.0,
                h: 8.0,
            },
            uv: Rect {
                x: 0.1,
                y: 0.2,
                w: 0.3,
                h: 0.4,
            },
            px_range: 8.0,
        };
        let inst = run.instance(&glyph);
        assert_eq!(inst.rect_pos, [5.0, 6.0]);
        assert_eq!(inst.rect_size, [7.0, 8.0]);
        assert_eq!(inst.uv_pos, [0.1, 0.2]);
        assert_eq!(inst.uv_size, [0.3, 0.4]);
        // The run's color, not per-glyph.
        assert_eq!(inst.color, [0.1, 0.2, 0.3, 0.9]);
        assert_eq!(inst.px_range, 8.0);
    }

    #[test]
    fn image_lowers_to_instance() {
        let img = ImageDraw {
            rect: Rect {
                x: 10.0,
                y: 20.0,
                w: 30.0,
                h: 40.0,
            },
            uv: Rect {
                x: 0.25,
                y: 0.5,
                w: 0.25,
                h: 0.5,
            },
            tint: Rgba {
                r: 1.0,
                g: 0.5,
                b: 0.25,
                a: 0.8,
            },
            texture: TextureId(0),
        };
        let inst = img.to_instance();
        assert_eq!(inst.rect_pos, [10.0, 20.0]);
        assert_eq!(inst.rect_size, [30.0, 40.0]);
        assert_eq!(inst.uv_pos, [0.25, 0.5]);
        assert_eq!(inst.uv_size, [0.25, 0.5]);
        assert_eq!(inst.color, [1.0, 0.5, 0.25, 0.8]);
    }

    #[test]
    fn rect_intersect_overlap_and_disjoint() {
        let a = Rect {
            x: 0.0,
            y: 0.0,
            w: 100.0,
            h: 100.0,
        };
        let b = Rect {
            x: 50.0,
            y: 40.0,
            w: 100.0,
            h: 100.0,
        };
        // Overlap: the common region.
        assert_eq!(
            a.intersect(b),
            Rect {
                x: 50.0,
                y: 40.0,
                w: 50.0,
                h: 60.0,
            }
        );
        // Disjoint: empty (w/h clamped to 0), never negative.
        let far = Rect {
            x: 200.0,
            y: 200.0,
            w: 10.0,
            h: 10.0,
        };
        let r = a.intersect(far);
        assert_eq!(r.w, 0.0);
        assert_eq!(r.h, 0.0);
    }

    #[test]
    fn quad_lowers_to_instance() {
        let q = Quad {
            rect: Rect {
                x: 10.0,
                y: 20.0,
                w: 30.0,
                h: 40.0,
            },
            color: Rgba {
                r: 1.0,
                g: 0.5,
                b: 0.25,
                a: 1.0,
            },
            radius: 4.0,
            border: Border {
                width: 2.0,
                color: Rgba {
                    r: 0.0,
                    g: 0.0,
                    b: 0.0,
                    a: 1.0,
                },
            },
        };
        let inst = q.to_instance();
        assert_eq!(inst.rect_pos, [10.0, 20.0]);
        assert_eq!(inst.rect_size, [30.0, 40.0]);
        assert_eq!(inst.color, [1.0, 0.5, 0.25, 1.0]);
        assert_eq!(inst.radius, 4.0);
        assert_eq!(inst.border_width, 2.0);
        assert_eq!(inst.border_color, [0.0, 0.0, 0.0, 1.0]);
    }
}
