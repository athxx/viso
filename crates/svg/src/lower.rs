//! [`Tree`](crate::tree::Tree) → [`Primitive`] lowering.

use viso_math::Srgb;
use viso_render::{
    DashPattern, LineCap, LineJoin, Path, PathCmd, Point, Primitive, Rgba, Stroke, StrokeAlign,
};

use crate::tree as svg;

/// Lower every representable path of `tree`, in paint order.
pub(crate) fn lower_tree(tree: &svg::Tree) -> Vec<Primitive> {
    let mut prims = Vec::new();
    walk_group(&tree.root, &svg::Transform::IDENTITY, 1.0, &mut prims);
    prims
}

/// Depth-first walk of a tree group, appending lowered primitives in paint
/// order. `parent` is the accumulated transform and `opacity` the accumulated
/// group opacity above this group.
fn walk_group(group: &svg::Group, parent: &svg::Transform, opacity: f32, out: &mut Vec<Primitive>) {
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
fn lower_path(path: &svg::Path, ts: &svg::Transform, opacity: f32) -> Option<Primitive> {
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
fn lower_geometry(data: &svg::PathData, ts: &svg::Transform) -> Vec<PathCmd> {
    let map = |p: svg::Point| {
        let q = ts.apply(p);
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
fn solid_stroke(stroke: &svg::Stroke, ts: &svg::Transform, opacity: f32) -> Option<Stroke> {
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
