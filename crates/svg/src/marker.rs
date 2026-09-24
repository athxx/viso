//! `marker-start` / `marker-mid` / `marker-end` (SVG 2 §11.6).
//!
//! Marker vertices are the source path commands' end points (from
//! [`CmdSpan`]s), so an arc is one vertex even though it becomes several
//! cubics. Orientation follows the path direction rules: the in/out tangent at
//! a vertex, their bisector for mid vertices and closed-subpath ends.

use std::sync::Arc;

use crate::convert::{Axis, Converter, State, rect_clip};
use crate::geom::{PathData, Point, Rect, Segment, Transform};
use crate::path_data::{CmdKind, CmdSpan};
use crate::syntax::{self, Length, Stream};
use crate::tree::*;

/// A marker vertex: position plus incoming/outgoing tangent directions.
#[derive(Debug, Clone, Copy)]
struct Vertex {
    p: Point,
    dir_in: Option<Point>,
    dir_out: Option<Point>,
    /// Start or end vertex of a closed subpath (oriented by the bisector).
    closed: bool,
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum Orient {
    Auto,
    AutoStartReverse,
    Angle(f32),
}

fn nonzero(v: Point) -> Option<Point> {
    (v.x != 0.0 || v.y != 0.0).then_some(v)
}

/// Start/end tangents and end point of each canonical segment.
struct SegGeom {
    to: Point,
    start: Option<Point>,
    end: Option<Point>,
}

fn segment_geometry(data: &PathData) -> Vec<SegGeom> {
    let mut out = Vec::with_capacity(data.len());
    let mut cur = Point::default();
    let mut sub_start = Point::default();
    for s in data.segments() {
        let g = match *s {
            Segment::MoveTo(p) => {
                sub_start = p;
                SegGeom {
                    to: p,
                    start: None,
                    end: None,
                }
            }
            Segment::LineTo(p) => {
                let d = nonzero(p - cur);
                SegGeom {
                    to: p,
                    start: d,
                    end: d,
                }
            }
            Segment::QuadTo(c, p) => SegGeom {
                to: p,
                start: nonzero(c - cur).or(nonzero(p - cur)),
                end: nonzero(p - c).or(nonzero(p - cur)),
            },
            Segment::CubicTo(c0, c1, p) => SegGeom {
                to: p,
                start: nonzero(c0 - cur).or(nonzero(c1 - cur)).or(nonzero(p - cur)),
                end: nonzero(p - c1).or(nonzero(p - c0)).or(nonzero(p - cur)),
            },
            Segment::Close => {
                let d = nonzero(sub_start - cur);
                SegGeom {
                    to: sub_start,
                    start: d,
                    end: d,
                }
            }
        };
        cur = g.to;
        out.push(g);
    }
    out
}

fn vertices(data: &PathData, spans: &[CmdSpan]) -> Vec<Vertex> {
    let geo = segment_geometry(data);
    let mut out: Vec<Vertex> = Vec::with_capacity(spans.len());
    // Index into `out` of the current subpath's first vertex.
    let mut sub_first = 0usize;
    for span in spans {
        if span.start >= span.end || span.end > geo.len() {
            continue;
        }
        let seg = &geo[span.start..span.end];
        let p = seg[seg.len() - 1].to;
        match span.kind {
            CmdKind::Move => {
                sub_first = out.len();
                out.push(Vertex {
                    p,
                    dir_in: None,
                    dir_out: None,
                    closed: false,
                });
            }
            CmdKind::Draw | CmdKind::Close => {
                let start = seg.iter().find_map(|g| g.start);
                let end = seg.iter().rev().find_map(|g| g.end);
                if let Some(prev) = out.last_mut() {
                    prev.dir_out = start.or(prev.dir_out);
                }
                out.push(Vertex {
                    p,
                    dir_in: end,
                    dir_out: None,
                    closed: false,
                });
                if span.kind == CmdKind::Close {
                    // The closing vertex coincides with the subpath start: it
                    // continues into the subpath's first segment.
                    let first_out = out.get(sub_first).and_then(|v| v.dir_out);
                    let last = out.len() - 1;
                    out[last].dir_out = first_out;
                    out[last].closed = true;
                    let closing_in = out[last].dir_in;
                    if let Some(first) = out.get_mut(sub_first) {
                        first.dir_in = closing_in;
                        first.closed = true;
                    }
                }
            }
        }
    }
    // Fill missing directions from the other side (degenerate segments).
    for v in &mut out {
        if v.dir_in.is_none() {
            v.dir_in = v.dir_out;
        }
        if v.dir_out.is_none() {
            v.dir_out = v.dir_in;
        }
    }
    out
}

fn angle(d: Option<Point>) -> f32 {
    d.map_or(0.0, |d| d.y.atan2(d.x).to_degrees())
}

/// Bisector angle of the in and out directions (degrees).
fn bisector(v: &Vertex) -> f32 {
    let a = angle(v.dir_in);
    let b = angle(v.dir_out);
    let mut d = b - a;
    if d > 180.0 {
        d -= 360.0;
    } else if d < -180.0 {
        d += 360.0;
    }
    a + d / 2.0
}

fn parse_orient(s: Option<&str>) -> Orient {
    let Some(s) = s.map(str::trim) else {
        return Orient::Angle(0.0);
    };
    match s {
        "auto" => Orient::Auto,
        "auto-start-reverse" => Orient::AutoStartReverse,
        _ => Orient::Angle(parse_angle(s).unwrap_or(0.0)),
    }
}

/// `<angle>`: a number with an optional `deg`/`grad`/`rad`/`turn` unit.
pub(crate) fn parse_angle(s: &str) -> Option<f32> {
    let mut st = Stream::new(s.trim());
    let n = st.parse_number()?;
    let deg = match st.rest().trim() {
        "" | "deg" => n,
        "grad" => n * 0.9,
        "rad" => n.to_degrees(),
        "turn" => n * 360.0,
        _ => return None,
    };
    Some(deg)
}

pub(crate) fn convert_markers(
    c: &mut Converter,
    data: &Arc<PathData>,
    spans: &[CmdSpan],
    st: &State,
    fill: &Option<Fill>,
    stroke: &Option<Stroke>,
) -> Vec<Node> {
    let s = &st.style;
    if s.marker_start.is_none() && s.marker_mid.is_none() && s.marker_end.is_none() {
        return Vec::new();
    }
    let verts = vertices(data, spans);
    if verts.is_empty() {
        return Vec::new();
    }
    let stroke_width = st.resolve(s.stroke_width, Axis::Diag);
    let context = (
        fill.as_ref().map(|f| f.paint.clone()),
        stroke.as_ref().map(|s| s.paint.clone()),
    );
    let mut out = Vec::new();
    let last = verts.len() - 1;
    let jobs = [
        (s.marker_start.clone(), 0..1usize, 0u8),
        (s.marker_mid.clone(), 1..last.max(1), 1),
        (s.marker_end.clone(), last..last + 1, 2),
    ];
    for (id, range, which) in jobs {
        let Some(id) = id else { continue };
        let Some(m) = c.sd.by_id(&id) else { continue };
        if c.name(m) != Some("marker") || c.stack.contains(&m) {
            continue;
        }
        for i in range {
            let v = &verts[i];
            let orient = parse_orient(c.attr(m, "orient"));
            let rot = match orient {
                Orient::Angle(a) => a,
                Orient::Auto | Orient::AutoStartReverse => {
                    let a = if which == 1 || v.closed {
                        bisector(v)
                    } else if which == 0 {
                        angle(v.dir_out)
                    } else {
                        angle(v.dir_in)
                    };
                    if which == 0 && orient == Orient::AutoStartReverse {
                        a + 180.0
                    } else {
                        a
                    }
                }
            };
            instance(c, m, v.p, rot, stroke_width, st, &context, &mut out);
        }
    }
    out
}

#[allow(clippy::too_many_arguments)]
fn instance(
    c: &mut Converter,
    m: crate::xml::NodeId,
    at: Point,
    rot: f32,
    stroke_width: f32,
    st: &State,
    context: &(Option<Paint>, Option<Paint>),
    out: &mut Vec<Node>,
) {
    let mst0 = c.sd.style_from_ancestors(m);
    if !mst0.display {
        return;
    }
    let mut mst = State {
        style: mst0,
        vp: st.vp,
        in_clip: false,
        context: Some(context.clone()),
    };
    let three = Length::new(3.0, syntax::Unit::None);
    let mw = c.length(m, "markerWidth", st, Axis::X, three);
    let mh = c.length(m, "markerHeight", st, Axis::Y, three);
    if !(mw > 0.0 && mh > 0.0) {
        return;
    }
    let view_box = c.attr(m, "viewBox").and_then(syntax::parse_view_box);
    if view_box.is_some_and(|v| !v.is_valid()) {
        return;
    }
    let aspect = c
        .attr(m, "preserveAspectRatio")
        .map(syntax::parse_aspect)
        .unwrap_or_default();
    let vb_ts = view_box.map_or(Transform::IDENTITY, |vb| {
        syntax::view_box_transform(vb, aspect, mw, mh)
    });
    if let Some(vb) = view_box {
        mst.vp = (vb.w, vb.h);
    }
    let ref_x = c.length(m, "refX", &mst, Axis::X, Length::ZERO);
    let ref_y = c.length(m, "refY", &mst, Axis::Y, Length::ZERO);
    let r = vb_ts.apply(Point::new(ref_x, ref_y));
    let scale = if c.attr(m, "markerUnits") == Some("userSpaceOnUse") {
        1.0
    } else {
        stroke_width
    };
    let ts = Transform::translate(at.x, at.y)
        .pre_concat(&Transform::rotate(rot))
        .pre_concat(&Transform::scale(scale, scale))
        .pre_concat(&Transform::translate(-r.x, -r.y));

    c.stack.push(m);
    let mut content = Group {
        transform: vb_ts,
        ..Group::default()
    };
    c.convert_children(m, &mst, &mut content.children);
    c.stack.pop();
    if content.children.is_empty() {
        return;
    }
    let clip = !mst.style.overflow_visible;
    let mut g = Group {
        transform: ts,
        ..Group::default()
    };
    if clip {
        g.clip_path = Some(Arc::new(rect_clip(Rect::new(0.0, 0.0, mw, mh))));
    }
    g.children.push(Node::Group(Box::new(content)));
    out.push(Node::Group(Box::new(g)));
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::path_data::parse_path;

    #[test]
    fn vertices_follow_source_commands() {
        let (p, spans) = parse_path("M0 0 L10 0 A5 5 0 0 1 10 10 Z");
        let v = vertices(&p, &spans);
        assert_eq!(v.len(), 4, "move, line, arc, close");
        assert_eq!(v[1].p, Point::new(10.0, 0.0));
        assert_eq!(angle(v[0].dir_out), 0.0);
        // The close vertex's in-direction points back to the start (-x, -y).
        let d = v[3].dir_in.unwrap();
        assert!(d.x < 0.0 && d.y < 0.0);
    }

    #[test]
    fn angles_parse_units() {
        assert_eq!(parse_angle("90"), Some(90.0));
        assert_eq!(parse_angle("0.5turn"), Some(180.0));
        assert_eq!(parse_angle("100grad"), Some(90.0));
        assert_eq!(
            parse_orient(Some("auto-start-reverse")),
            Orient::AutoStartReverse
        );
    }
}
