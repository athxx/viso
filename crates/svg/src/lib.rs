//! `viso-svg` — the SVG input lane (§13).
//!
//! SVG is an **input format**, not a per-frame renderer: bytes are parsed once
//! into a normalized vector scene and lowered to the same retained
//! [`viso_render::Primitive`] stream every other source produces, so the result
//! is a cacheable Render IR input rather than a live XML DOM. [`parse_svg`] is
//! the whole public surface — a synchronous, allocation-bounded CPU pass. Static
//! assets can run it at build time and dynamic assets on a worker thread (§26);
//! the call site owns that policy, this crate just does the transform.
//!
//! usvg does the standards-heavy work — XML/CSS parsing, `viewBox`, unit
//! resolution, `<use>` expansion, gradient/group flattening, and resolving every
//! node's absolute transform (§3.7: own the integration, reuse the proven codec).
//! The walk here is thin: each usvg `Path` node becomes one
//! [`viso_render::Path`], with its absolute transform *baked into the flattened
//! coordinates* (an SVG transform is a parse-time one-shot, never a per-frame
//! matrix), its solid fill/stroke paint converted to straight-linear
//! [`Rgba`](viso_render::Rgba), and its stroke width/cap/join/miter/dash mapped
//! onto [`viso_render::Stroke`].
//!
//! Scope this round: paths with solid fill and/or solid stroke (plus everything
//! usvg has already flattened — nested groups, transforms, `viewBox`, `<use>`).
//! Gradient/pattern paints, filters, clip-paths, images, and text are recognized
//! and skipped rather than mis-rendered; they land as usvg's capabilities are
//! mapped onto `Brush` in later rounds.

#![forbid(unsafe_op_in_unsafe_fn)]

use std::fmt;

use usvg::tiny_skia_path::{PathSegment, Transform};
use viso_math::{Point as MPoint, Rect, Srgb};
use viso_render::{
    DashPattern, LineCap, LineJoin, Path, PathCmd, Point, Primitive, Rgba, Stroke, StrokeAlign,
};

/// A parsed SVG document lowered to a retained primitive scene.
///
/// `prims` is in document paint order (a group's children after the group's
/// earlier siblings), ready to hand to the renderer or cache as Render IR.
/// `size` is the document's intrinsic size in its user-space units (the resolved
/// `width`/`height`, i.e. the `viewBox` mapped through any outer dimensions),
/// the natural layout box for the asset.
#[derive(Debug, Clone, PartialEq)]
pub struct SvgScene {
    /// The lowered primitives, in paint order.
    pub prims: Vec<Primitive>,
    /// Intrinsic document size `(width, height)` in user-space units.
    pub size: (f32, f32),
}

impl SvgScene {
    /// Axis-aligned bounding box of all drawable content, in the same
    /// physical-pixel space as [`size`](Self::size), or `None` when the scene
    /// has no lowered paths (an empty document, or the gradient/pattern-only
    /// paints this round skips).
    ///
    /// Distinct from `size`, which is the usvg *document* box: this is a box
    /// over every path's baked anchor and Bézier control points. Including the
    /// control points makes it a conservative bound — the control hull contains
    /// the curve, so a bulging curve is never under-reported. The fold runs
    /// through [`viso_math::point_bounds`] over one contiguous point array, the
    /// SIMD-accelerated min/max reduction (§13.6); the strided post-stroke
    /// `geo_bounds` fold in `viso-render` stays scalar and points contiguous
    /// callers such as this one here.
    pub fn content_bounds(&self) -> Option<Rect> {
        let mut pts: Vec<MPoint> = Vec::new();
        for prim in &self.prims {
            if let Primitive::Path(path) = prim {
                for cmd in &path.cmds {
                    match *cmd {
                        PathCmd::MoveTo(p) | PathCmd::LineTo(p) => {
                            pts.push(MPoint::new(p.x, p.y));
                        }
                        PathCmd::QuadTo(c, p) => {
                            pts.push(MPoint::new(c.x, c.y));
                            pts.push(MPoint::new(p.x, p.y));
                        }
                        PathCmd::CubicTo(c0, c1, p) => {
                            pts.push(MPoint::new(c0.x, c0.y));
                            pts.push(MPoint::new(c1.x, c1.y));
                            pts.push(MPoint::new(p.x, p.y));
                        }
                        PathCmd::Close => {}
                    }
                }
            }
        }
        if pts.is_empty() {
            return None;
        }
        Some(viso_math::point_bounds(&pts))
    }
}

/// Why an SVG failed to parse or lower.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SvgError {
    /// usvg could not parse the bytes as a valid SVG document.
    Parse(String),
}

impl fmt::Display for SvgError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            SvgError::Parse(msg) => write!(f, "invalid SVG: {msg}"),
        }
    }
}

impl std::error::Error for SvgError {}

/// Parse SVG `bytes` and lower them to a retained [`SvgScene`].
///
/// A synchronous CPU pass: parse with usvg, then walk the resolved node tree
/// depth-first in paint order, emitting one [`Primitive::Path`] per solid
/// filled/stroked path node. Curves are kept as `Path` curve commands (the
/// tessellator flattens them); the node's absolute transform is baked into every
/// coordinate. Paths with no solid paint we can represent are skipped.
pub fn parse_svg(bytes: &[u8]) -> Result<SvgScene, SvgError> {
    let options = usvg::Options::default();
    let tree =
        usvg::Tree::from_data(bytes, &options).map_err(|e| SvgError::Parse(e.to_string()))?;

    let size = tree.size();
    let mut prims = Vec::new();
    walk_group(tree.root(), &mut prims);

    Ok(SvgScene {
        prims,
        size: (size.width(), size.height()),
    })
}

/// Depth-first walk of a usvg group, appending lowered primitives in paint order.
///
/// Group transforms are already folded into each child's `abs_transform`, so the
/// walk carries no matrix of its own; it only recurses into subgroups and lowers
/// path nodes. Non-path nodes (images, text) and groups that only exist for
/// clip/filter/opacity compositing are traversed for their path children but
/// otherwise skipped this round.
fn walk_group(group: &usvg::Group, out: &mut Vec<Primitive>) {
    for node in group.children() {
        match node {
            usvg::Node::Group(g) => walk_group(g, out),
            usvg::Node::Path(p) => {
                if let Some(prim) = lower_path(p) {
                    out.push(prim);
                }
            }
            // Images and text are their own lanes; a group used purely for
            // clip/filter compositing still reaches its path children above.
            usvg::Node::Image(_) | usvg::Node::Text(_) => {}
        }
    }
}

/// Lower a single usvg path node to a [`Primitive::Path`], or `None` when it has
/// no solid fill or stroke we can represent (a gradient/pattern-only paint, or a
/// zero-area invisible node). The node's absolute transform is baked into the
/// emitted coordinates.
fn lower_path(path: &usvg::Path) -> Option<Primitive> {
    let fill = path.fill().and_then(solid_fill);
    let stroke = path.stroke().and_then(solid_stroke);
    if fill.is_none() && stroke.is_none() {
        return None;
    }

    let xform = path.abs_transform();
    let cmds = lower_geometry(path.data(), xform);
    if cmds.is_empty() {
        return None;
    }

    Some(Primitive::Path(Path { cmds, fill, stroke }))
}

/// Convert a `tiny_skia_path::Path` to Viso [`PathCmd`]s, transforming every
/// control/anchor point through `xform` (the node's baked absolute transform).
fn lower_geometry(data: &usvg::tiny_skia_path::Path, xform: Transform) -> Vec<PathCmd> {
    let mut cmds = Vec::new();
    for seg in data.segments() {
        match seg {
            PathSegment::MoveTo(p) => cmds.push(PathCmd::MoveTo(map(p, xform))),
            PathSegment::LineTo(p) => cmds.push(PathCmd::LineTo(map(p, xform))),
            PathSegment::QuadTo(c, p) => cmds.push(PathCmd::QuadTo(map(c, xform), map(p, xform))),
            PathSegment::CubicTo(c0, c1, p) => cmds.push(PathCmd::CubicTo(
                map(c0, xform),
                map(c1, xform),
                map(p, xform),
            )),
            PathSegment::Close => cmds.push(PathCmd::Close),
        }
    }
    cmds
}

/// Apply the baked absolute transform to one path point and convert to a render
/// [`Point`] (physical-pixel, top-left origin — the same space usvg resolves to).
#[inline]
fn map(mut p: usvg::tiny_skia_path::Point, xform: Transform) -> Point {
    xform.map_point(&mut p);
    Point::new(p.x, p.y)
}

/// The solid straight-linear color of a usvg fill, or `None` for a
/// gradient/pattern paint (deferred). The fill's own opacity multiplies the
/// paint alpha.
fn solid_fill(fill: &usvg::Fill) -> Option<Rgba> {
    solid_color(fill.paint(), fill.opacity())
}

/// The solid stroke of a usvg stroke node mapped to a [`Stroke`], or `None` for a
/// gradient/pattern paint. Width, caps, joins, miter limit, and dash pattern are
/// carried across; the stroke sits centered on the outline (SVG's model).
fn solid_stroke(stroke: &usvg::Stroke) -> Option<Stroke> {
    let color = solid_color(stroke.paint(), stroke.opacity())?;
    Some(Stroke {
        width: stroke.width().get(),
        color,
        cap: map_cap(stroke.linecap()),
        join: map_join(stroke.linejoin()),
        miter_limit: stroke.miterlimit().get(),
        align: StrokeAlign::Center,
        dash: map_dash(stroke.dasharray(), stroke.dashoffset()),
        hairline: false,
    })
}

/// Resolve a usvg [`Paint`](usvg::Paint) to straight-linear [`Rgba`] when it is a
/// solid color, applying `opacity` (a normalized `[0,1]`) to the alpha. Gradient
/// and pattern paints return `None` (deferred to a later round).
fn solid_color(paint: &usvg::Paint, opacity: usvg::Opacity) -> Option<Rgba> {
    match paint {
        usvg::Paint::Color(c) => {
            let alpha = (opacity.get() * 255.0).round() as u8;
            Some(Srgb::from_u8(c.red, c.green, c.blue, alpha).into_linear_straight())
        }
        usvg::Paint::LinearGradient(_)
        | usvg::Paint::RadialGradient(_)
        | usvg::Paint::Pattern(_) => None,
    }
}

/// Map a usvg dash array + offset to a [`DashPattern`], clamped to
/// [`DASH_SEGMENTS_MAX`] run lengths. An empty or absent array is a solid stroke.
fn map_dash(dashes: Option<&[f32]>, offset: f32) -> Option<DashPattern> {
    let dashes = dashes?;
    if dashes.is_empty() {
        return None;
    }
    Some(DashPattern::new(dashes, offset))
}

/// usvg line cap → render [`LineCap`].
fn map_cap(cap: usvg::LineCap) -> LineCap {
    match cap {
        usvg::LineCap::Butt => LineCap::Butt,
        usvg::LineCap::Round => LineCap::Round,
        usvg::LineCap::Square => LineCap::Square,
    }
}

/// usvg line join → render [`LineJoin`]. `MiterClip` (SVG 2's clipped miter) has
/// no distinct render variant and maps to a plain miter.
fn map_join(join: usvg::LineJoin) -> LineJoin {
    match join {
        usvg::LineJoin::Miter | usvg::LineJoin::MiterClip => LineJoin::Miter,
        usvg::LineJoin::Round => LineJoin::Round,
        usvg::LineJoin::Bevel => LineJoin::Bevel,
    }
}
