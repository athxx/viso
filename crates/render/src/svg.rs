//! SVG as the renderer draws it: a document parsed by `viso-svg` into its
//! normalized tree, lowered to the same retained [`Primitive`] stream every
//! other source produces.
//!
//! Lowering is thin: each tree `Path` becomes one [`Path`], with its
//! accumulated transform *baked into the coordinates* (an SVG transform is a
//! parse-time one-shot, never a per-frame matrix) and into the stroke
//! width/dashes (by the transform's mean scale), its solid fill/stroke paint
//! converted to straight-linear [`Rgba`], and its stroke cap/join/miter mapped
//! onto [`Stroke`].
//!
//! Scope this round: paths with solid fill and/or solid stroke. Group opacity is
//! multiplied into each descendant's paint alpha (exact for non-overlapping
//! content). Gradient/pattern paints, images, and text are skipped rather than
//! mis-rendered; clip-paths, masks, and filters are ignored (their content
//! paths still render) until the renderer grows layers.

use viso_math::{Point as MPoint, Rect};

use crate::asset::{
    AssetBytes, AssetError, AssetFormat, AssetKind, DecodedAsset, decode_raster_asset,
};

pub use viso_svg::{SvgError, tree};

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
    /// Distinct from `size`, which is the *document* box: this is a box
    /// over every path's baked anchor and Bézier control points. Including the
    /// control points makes it a conservative bound — the control hull contains
    /// the curve, so a bulging curve is never under-reported. The fold runs
    /// through [`viso_math::point_bounds`] over one contiguous point array, the
    /// SIMD-accelerated min/max reduction (§13.6); the strided post-stroke
    /// `geo_bounds` fold in the renderer stays scalar and points contiguous
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

impl SvgScene {
    /// The scene as a vector asset: its paths and its document size, which
    /// [`VectorScene::place`](crate::asset::VectorScene::place) stretches
    /// into a node's box.
    pub fn into_vector(self) -> crate::asset::VectorScene {
        let paths: Vec<crate::primitive::Path> = self
            .prims
            .into_iter()
            .filter_map(|prim| match prim {
                Primitive::Path(path) => Some(path),
                _ => None,
            })
            .collect();
        crate::asset::VectorScene {
            paths: paths.into(),
            size: [self.size.0, self.size.1],
        }
    }
}

/// Decodes an asset by the format its bytes are in: a PNG or JPEG to a
/// raster, an SVG to a vector scene, and an image the build converted to a
/// raster whose texels it borrows from `bytes`.
///
/// # Errors
///
/// Bytes in no asset format, or that their decoder refuses.
pub fn decode_asset(bytes: &AssetBytes) -> Result<DecodedAsset, AssetError> {
    if viso_image::texture::is_texture(bytes) {
        return decode_raster_asset(bytes).map(DecodedAsset::Raster);
    }
    match AssetFormat::sniff(bytes).map(AssetFormat::kind) {
        Some(AssetKind::Raster) => decode_raster_asset(bytes).map(DecodedAsset::Raster),
        Some(AssetKind::Vector) => parse_svg(bytes)
            .map(|scene| DecodedAsset::Vector(scene.into_vector()))
            .map_err(|error| AssetError::Decode(error.to_string())),
        None => Err(AssetError::Format),
    }
}

/// Parse SVG `bytes` and lower them to a retained [`SvgScene`].
///
/// A synchronous CPU pass: parse with [`viso_svg::parse`], then walk the tree
/// depth-first in paint order, emitting one [`Primitive::Path`] per solid
/// filled/stroked path. Curves are kept as `Path` curve commands (the
/// tessellator flattens them); the accumulated transform is baked into every
/// coordinate. Paths with no solid paint we can represent are skipped.
pub fn parse_svg(bytes: &[u8]) -> Result<SvgScene, SvgError> {
    let tree = viso_svg::parse(bytes)?;
    Ok(SvgScene {
        prims: lower_tree(&tree),
        size: tree.size,
    })
}

use crate::primitive::{
    DashPattern, LineCap, LineJoin, Path, PathCmd, Point, Primitive, Rgba, Stroke, StrokeAlign,
};
use viso_math::Srgb;

use viso_svg::tree as svg;

/// Lower every representable path of `tree`, in paint order.
pub(crate) fn lower_tree(tree: &svg::Tree) -> Vec<Primitive> {
    let mut prims = Vec::new();
    walk_group(&tree.root, &svg::Affine2::IDENTITY, 1.0, &mut prims);
    prims
}

/// Depth-first walk of a tree group, appending lowered primitives in paint
/// order. `parent` is the accumulated transform and `opacity` the accumulated
/// group opacity above this group.
fn walk_group(group: &svg::Group, parent: &svg::Affine2, opacity: f32, out: &mut Vec<Primitive>) {
    let ts = parent.pre_concat(&group.transform);
    let opacity = opacity * group.opacity;
    for node in &group.children {
        match node {
            svg::Node::Group(g) => walk_group(g, &ts, opacity, out),
            svg::Node::Path(p) => {
                if let Some(prim) = lower_path(p, &ts, opacity) {
                    out.push(prim);
                }
            }
            // Images are their own lane.
            svg::Node::Image(_) => {}
        }
    }
}

/// Lower a single tree path to a [`Primitive::Path`], or `None` when it has no
/// solid fill or stroke we can represent (a gradient/pattern-only paint).
fn lower_path(path: &svg::Path, ts: &svg::Affine2, opacity: f32) -> Option<Primitive> {
    let fill = path
        .fill
        .as_ref()
        .and_then(|f| solid_color(&f.paint, f.opacity * opacity));
    let stroke = path
        .stroke
        .as_ref()
        .and_then(|s| solid_stroke(s, ts, opacity));
    if fill.is_none() && stroke.is_none() {
        return None;
    }
    let cmds = lower_geometry(&path.data, ts);
    if cmds.is_empty() {
        return None;
    }
    Some(Primitive::Path(Path {
        cmds,
        fill,
        stroke,
        shadow: None,
    }))
}

/// Convert tree path data to Viso [`PathCmd`]s, transforming every
/// control/anchor point through `ts`.
fn lower_geometry(data: &svg::PathData, ts: &svg::Affine2) -> Vec<PathCmd> {
    let map = |p: svg::Vec2| {
        let q = ts.transform_point(p);
        Point::new(q.x, q.y)
    };
    data.segments()
        .iter()
        .map(|seg| match *seg {
            svg::Segment::MoveTo(p) => PathCmd::MoveTo(map(p)),
            svg::Segment::LineTo(p) => PathCmd::LineTo(map(p)),
            svg::Segment::QuadTo(c, p) => PathCmd::QuadTo(map(c), map(p)),
            svg::Segment::CubicTo(c0, c1, p) => PathCmd::CubicTo(map(c0), map(c1), map(p)),
            svg::Segment::Close => PathCmd::Close,
        })
        .collect()
}

/// The solid stroke of a tree path mapped to a [`Stroke`], or `None` for a
/// gradient/pattern paint. Width and dashes are scaled by the baked transform
/// (unless `vector-effect: non-scaling-stroke`); the stroke sits centered on
/// the outline (SVG's model).
fn solid_stroke(stroke: &svg::Stroke, ts: &svg::Affine2, opacity: f32) -> Option<Stroke> {
    let color = solid_color(&stroke.paint, stroke.opacity * opacity)?;
    let scale = if stroke.non_scaling {
        1.0
    } else {
        ts.mean_scale()
    };
    let dash = stroke.dasharray.as_ref().map(|d| {
        let d: Vec<f32> = d.iter().map(|v| v * scale).collect();
        DashPattern::new(&d, stroke.dashoffset * scale)
    });
    Some(Stroke {
        width: stroke.width * scale,
        color,
        cap: map_cap(stroke.cap),
        join: map_join(stroke.join),
        miter_limit: stroke.miter_limit,
        align: StrokeAlign::Center,
        dash,
        hairline: false,
    })
}

/// Resolve a tree [`Paint`](svg::Paint) to straight-linear [`Rgba`] when it is
/// a solid color, multiplying its alpha by `opacity` (`[0,1]`). Gradient and
/// pattern paints return `None` (deferred to a later round).
fn solid_color(paint: &svg::Paint, opacity: f32) -> Option<Rgba> {
    match paint {
        svg::Paint::Color(c) => {
            let alpha = (c.a as f32 * opacity).round().clamp(0.0, 255.0) as u8;
            Some(Srgb::from_u8(c.r, c.g, c.b, alpha).into_linear_straight())
        }
        _ => None,
    }
}

fn map_cap(cap: svg::LineCap) -> LineCap {
    match cap {
        svg::LineCap::Butt => LineCap::Butt,
        svg::LineCap::Round => LineCap::Round,
        svg::LineCap::Square => LineCap::Square,
    }
}

/// `miter-clip`/`arcs` (SVG 2) have no distinct render variant and map to a
/// plain miter.
fn map_join(join: svg::LineJoin) -> LineJoin {
    match join {
        svg::LineJoin::Round => LineJoin::Round,
        svg::LineJoin::Bevel => LineJoin::Bevel,
        _ => LineJoin::Miter,
    }
}
